/*
   +----------------------------------------------------------------------+
   | HipHop for PHP                                                       |
   +----------------------------------------------------------------------+
   | Copyright (c) 2010-present Facebook, Inc. (http://www.facebook.com)  |
   +----------------------------------------------------------------------+
   | This source file is subject to version 3.01 of the PHP license,      |
   | that is bundled with this package in the file LICENSE, and is        |
   | available through the world-wide-web at the following url:           |
   | http://www.php.net/license/3_01.txt                                  |
   | If you did not receive a copy of the PHP license and are unable to   |
   | obtain it through the world-wide-web, please send a note to          |
   | license@php.net so we can mail you a copy immediately.               |
   +----------------------------------------------------------------------+
*/

#include "hphp/runtime/vm/jit/service-request-handlers.h"

#include "hphp/runtime/ext/asio/ext_static-wait-handle.h"

#include "hphp/runtime/vm/jit/code-cache.h"
#include "hphp/runtime/vm/jit/mcgen.h"
#include "hphp/runtime/vm/jit/perf-counters.h"
#include "hphp/runtime/vm/jit/prof-data.h"
#include "hphp/runtime/vm/jit/service-requests.h"
#include "hphp/runtime/vm/jit/smashable-instr.h"
#include "hphp/runtime/vm/jit/stub-alloc.h"
#include "hphp/runtime/vm/jit/tc.h"
#include "hphp/runtime/vm/jit/translator-inline.h"
#include "hphp/runtime/vm/jit/unwind-itanium.h"
#include "hphp/runtime/vm/jit/write-lease.h"

#include "hphp/runtime/vm/hhbc.h"
#include "hphp/runtime/vm/resumable.h"
#include "hphp/runtime/vm/runtime.h"
#include "hphp/runtime/vm/treadmill.h"
#include "hphp/runtime/vm/workload-stats.h"

#include "hphp/ppc64-asm/decoded-instr-ppc64.h"
#include "hphp/vixl/a64/decoder-a64.h"

#include "hphp/util/arch.h"
#include "hphp/util/ringbuffer.h"
#include "hphp/util/trace.h"

TRACE_SET_MOD(mcg);

namespace HPHP { namespace jit { namespace svcreq {

namespace {

///////////////////////////////////////////////////////////////////////////////

RegionContext getContext(SrcKey sk, bool profiling) {
  RegionContext ctx { sk, liveSpOff() };

  auto const func = sk.func();
  auto const fp = vmfp();
  auto const sp = vmsp();

  always_assert(func == fp->func());
  auto const ctxClass = func->cls();
  auto const addLiveType = [&](Location loc, tv_rval tv) {
    auto const type = typeFromTV(tv, ctxClass);
    ctx.liveTypes.push_back({loc, type});

    FTRACE(2, "Added live type: {}\n", show(ctx.liveTypes.back()));
  };

  // Track local types.
  for (uint32_t i = 0; i < func->numLocals(); ++i) {
    addLiveType(Location::Local{i}, frame_local(fp, i));
  }

  // Track stack types.
  int32_t stackOff = 0;
  visitStackElems(
    fp, sp,
    [&] (const TypedValue* tv) {
      addLiveType(Location::Stack{ctx.spOffset - stackOff}, tv);
      stackOff++;
    }
  );

  // Get the bytecode for `ctx', skipping Asserts.
  auto const op = [&] {
    auto pc = func->at(sk.offset());
    while (isTypeAssert(peek_op(pc))) {
      pc += instrLen(pc);
    }
    return peek_op(pc);
  }();
  assertx(!isTypeAssert(op));

  // Track the mbase type.  The member base register is only valid after a
  // member base op and before a member final op---and only AssertRAT*'s are
  // allowed to intervene in a sequence of bytecode member operations.
  if (isMemberDimOp(op) || isMemberFinalOp(op)) {
    auto const mbase = vmMInstrState().base;
    assertx(mbase != nullptr);
    addLiveType(Location::MBase{}, mbase);
  }

  return ctx;
}

const StaticString s_AlwaysInterp("__ALWAYS_INTERP");

/*
 * Create a translation for the SrcKey specified in args.
 *
 * If a translation for this SrcKey already exists it will be returned. The kind
 * of translation created will be selected based on the SrcKey specified.
 */
TranslationResult getTranslation(SrcKey sk) {
  sk.func()->validate();

  if (!RID().getJit()) {
    SKTRACE(2, sk, "punting because jit was disabled\n");
    return TranslationResult::failTransiently();
  }

  if (auto const sr = tc::findSrcRec(sk)) {
    if (auto const tca = sr->getTopTranslation()) {
      SKTRACE(2, sk, "getTranslation: found %p\n", tca);
      return TranslationResult{tca};
    }
  }

  if (UNLIKELY(RID().isJittingDisabled())) {
    SKTRACE(2, sk, "punting because jitting code was disabled\n");
    return TranslationResult::failTransiently();
  }

  if (UNLIKELY(!RO::RepoAuthoritative && sk.unit()->isCoverageEnabled())) {
    assertx(RO::EvalEnablePerFileCoverage);
    SKTRACE(2, sk, "punting because per file code coverage is enabled\n");
    return TranslationResult::failTransiently();
  }

  if (UNLIKELY(!RO::EvalHHIRAlwaysInterpIgnoreHint &&
               sk.func()->userAttributes().count(s_AlwaysInterp.get()))) {
    SKTRACE(2, sk,
            "punting because function is annotated with __ALWAYS_INTERP\n");
    return TranslationResult::failTransiently();
  }

  auto args = TransArgs{sk};
  args.kind = tc::profileFunc(args.sk.func()) ?
    TransKind::Profile : TransKind::Live;

  if (auto const s = tc::shouldTranslate(args.sk, args.kind);
      s != TranslationResult::Scope::Success) {
    return TranslationResult{s};
  }

  LeaseHolder writer(sk.func(), args.kind);
  if (!writer) return TranslationResult::failTransiently();

  if (auto const s = tc::shouldTranslate(args.sk, args.kind);
      s != TranslationResult::Scope::Success) {
    return TranslationResult{s};
  }

  tc::createSrcRec(sk, liveSpOff());

  if (auto const tca = tc::findSrcRec(sk)->getTopTranslation()) {
    // Handle extremely unlikely race; someone may have just added the first
    // translation for this SrcRec while we did a non-blocking wait on the
    // write lease in createSrcRec().
    return TranslationResult{tca};
  }

  return mcgen::retranslate(
    args,
    getContext(args.sk, args.kind == TransKind::Profile)
  );
}

/*
 * Runtime service handler that patches a jmp to the translation of u:dest from
 * toSmash.
 */
TranslationResult bindJmp(TCA toSmash,
                          SrcKey destSk,
                          FPInvOffset spOff,
                          ServiceRequest req,
                          TCA oldStub,
                          bool& smashed) {
  auto const result = getTranslation(destSk);
  if (!result.addr()) {
    if (result.isProcessPersistentFailure()) {
      // If we couldn't make a new translation, and we won't be able
      // to make any new translations for the remainder of the process
      // lifetime, we can just burn in a call to
      // handleResumeNoTranslate.
      if (auto const stub = emit_interp_no_translate_stub(spOff, destSk)) {
        // We still need to create a SrcRec (if one doesn't already
        // exist) to manage the locking correctly. This can fail.
        if (!tc::createSrcRec(destSk, liveSpOff(), true)) return result;
        if (req == REQ_BIND_ADDR) {
          tc::bindAddrToStub(toSmash, oldStub, stub, destSk, smashed);
        } else {
          assertx(req == REQ_BIND_JMP);
          tc::bindJmpToStub(toSmash, oldStub, stub, destSk, smashed);
        }
      }
    }
    return result;
  }

  if (req == REQ_BIND_ADDR) {
    if (auto const addr = tc::bindAddr(toSmash, destSk, smashed)) {
      return TranslationResult{addr};
    }
  } else {
    assertx(req == REQ_BIND_JMP);
    if (auto const addr = tc::bindJmp(toSmash, destSk, smashed)) {
      return TranslationResult{addr};
    }
  }

  return TranslationResult::failTransiently();
}

///////////////////////////////////////////////////////////////////////////////

}

TCA getFuncBody(const Func* func) {
  auto tca = func->getFuncBody();
  if (tca != nullptr) return tca;

  LeaseHolder writer(func, TransKind::Profile);
  if (!writer) return tc::ustubs().resumeHelper;

  tca = func->getFuncBody();
  if (tca != nullptr) return tca;

  if (func->numRequiredParams() != func->numNonVariadicParams()) {
    tca = tc::ustubs().resumeHelper;
    const_cast<Func*>(func)->setFuncBody(tca);
  } else {
    SrcKey sk{func, func->base(), ResumeMode::None};
    auto const trans = getTranslation(sk);
    tca = trans.isRequestPersistentFailure()
      ? tc::ustubs().interpHelperNoTranslate
      : trans.addr();
    if (trans.isProcessPersistentFailure()) {
      const_cast<Func*>(func)->setFuncBody(tca);
    }
  }

  return tca != nullptr ? tca : tc::ustubs().resumeHelper;
}

TCA handleServiceRequest(ReqInfo& info) noexcept {
  FTRACE(1, "handleServiceRequest {}\n", svcreq::to_name(info.req));

  assert_native_stack_aligned();
  tl_regState = VMRegState::CLEAN; // partially a lie: vmpc() isn't synced

  if (Trace::moduleEnabled(Trace::ringbuffer, 1)) {
    Trace::ringbufferEntry(
      Trace::RBTypeServiceReq, (uint64_t)info.req, (uint64_t)info.args[0].tca
    );
  }

  folly::Optional<TranslationResult> transResult;
  SrcKey sk;
  auto smashed = false;

  switch (info.req) {
    case REQ_BIND_JMP:
    case REQ_BIND_ADDR: {
      auto const toSmash = info.args[0].tca;
      sk = SrcKey::fromAtomicInt(info.args[1].sk);
      transResult =
        bindJmp(toSmash, sk, liveSpOff(), info.req, info.stub, smashed);
      break;
    }

    case REQ_RETRANSLATE: {
      INC_TPC(retranslate);
      sk = SrcKey{ liveFunc(), info.args[0].offset, liveResumeMode() };
      auto const context = getContext(sk, tc::profileFunc(sk.func()));
      transResult = mcgen::retranslate(TransArgs{sk}, context);
      SKTRACE(2, sk, "retranslated @%p\n", transResult->addr());
      break;
    }

    case REQ_RETRANSLATE_OPT: {
      sk = SrcKey::fromAtomicInt(info.args[0].sk);
      if (mcgen::retranslateOpt(sk.funcID())) {
        // Retranslation was successful. Resume execution at the new Optimize
        // translation.
        vmpc() = sk.func()->at(sk.offset());
        transResult = TranslationResult{tc::ustubs().resumeHelper};
      } else {
        // Retranslation failed, probably because we couldn't get the write
        // lease. Interpret a BB before running more Profile translations, to
        // avoid spinning through this path repeatedly.
        transResult = TranslationResult::failTransiently();
      }
      break;
    }

    case REQ_POST_INTERP_RET:
    case REQ_POST_INTERP_RET_GENITER: {
      // This is only responsible for the control-flow aspect of the Ret:
      // getting to the destination's translation, if any.
      auto const ar = info.args[0].ar;
      auto const caller = info.args[1].ar;
      UNUSED auto const func = ar->func();
      auto const callOff = ar->callOffset();
      auto const isAER = ar->isAsyncEagerReturn();
      assertx(caller == vmfp());
      auto const destFunc = caller->func();
      // Set PC so logging code in getTranslation doesn't get confused.
      vmpc() = skipCall(destFunc->at(destFunc->base() + callOff));
      if (info.req == REQ_POST_INTERP_RET) {
        TypedValue rv;
        rv.m_data = info.args[2].tvData;
        rv.m_type = info.args[3].tvType;
        rv.m_aux = info.args[3].tvAux;
        *ar->retSlot() = rv;
      }
      if (isAER) {
        // When returning to the interpreted FCall, the execution continues at
        // the next opcode, not honoring the request for async eager return.
        // If the callee returned eagerly, we need to wrap the result into
        // StaticWaitHandle.
        assertx(ar->retSlot()->m_aux.u_asyncEagerReturnFlag + 1 < 2);
        if (ar->retSlot()->m_aux.u_asyncEagerReturnFlag) {
          auto const retval = tvAssertPlausible(*ar->retSlot());
          auto const waitHandle = c_StaticWaitHandle::CreateSucceeded(retval);
          tvCopy(make_tv<KindOfObject>(waitHandle), *ar->retSlot());
        }
      }
      assertx(caller == vmfp());
      FTRACE(3, "REQ_POST_INTERP_RET{}: from {} to {}\n",
             info.req == REQ_POST_INTERP_RET ? "" : "_GENITER",
             func->fullName()->data(),
             destFunc->fullName()->data());
      sk = liveSK();
      transResult = getTranslation(sk);
      break;
    }
  }

  if (smashed && info.stub) {
    auto const stub = info.stub;
    FTRACE(3, "Freeing stub {} on treadmill\n", stub);
    Treadmill::enqueue([stub] { tc::freeTCStub(stub); });
  }

  auto const start = [&] {
    assertx(transResult);
    if (auto const addr = transResult->addr()) return addr;
    vmpc() = sk.pc();
    return transResult->scope() == TranslationResult::Scope::Transient
      ? tc::ustubs().interpHelperSyncedPC
      : tc::ustubs().interpHelperNoTranslate;
  }();

  if (Trace::moduleEnabled(Trace::ringbuffer, 1)) {
    auto skData = sk.valid() ? sk.toAtomicInt() : uint64_t(-1LL);
    Trace::ringbufferEntry(Trace::RBTypeResumeTC, skData, (uint64_t)start);
  }

  tl_regState = VMRegState::DIRTY;
  return start;
}

TCA handleBindCall(TCA toSmash, Func* func, int32_t numArgs) {
  TRACE(2, "bindCall %s, numArgs %d\n", func->fullName()->data(), numArgs);
  auto const trans = mcgen::getFuncPrologue(func, numArgs);
  TRACE(2, "bindCall immutably %s -> %p\n",
        func->fullName()->data(), trans.addr());

  if (trans.isProcessPersistentFailure()) {
    // We can't get a translation for this and we can't create any new
    // ones any longer. Smash the call site with a stub which will
    // interp the prologue, then run resumeHelperNoTranslate.
    tc::bindCall(
      toSmash,
      tc::ustubs().fcallHelperNoTranslateThunk,
      func,
      numArgs
    );
    return tc::ustubs().fcallHelperNoTranslateThunk;
  } else if (trans.addr()) {
    // Using start is racy but bindCall will recheck the start address after
    // acquiring a lock on the ProfTransRec
    tc::bindCall(toSmash, trans.addr(), func, numArgs);
    return trans.addr();
  } else {
    // We couldn't get a prologue address. Return a stub that will finish
    // entering the callee frame in C++, then call handleResume at the callee's
    // entry point.
    return trans.isRequestPersistentFailure()
      ? tc::ustubs().fcallHelperNoTranslateThunk
      : tc::ustubs().fcallHelperThunk;
  }
}

TCA handleResume(bool interpFirst) {
  assert_native_stack_aligned();
  FTRACE(1, "handleResume({})\n", interpFirst);

  if (!vmRegsUnsafe().pc) return tc::ustubs().callToExit;

  tl_regState = VMRegState::CLEAN;

  auto sk = liveSK();
  FTRACE(2, "handleResume: sk: {}\n", showShort(sk));

  TCA start;
  if (interpFirst) {
    start = nullptr;
    INC_TPC(interp_bb_force);
  } else {
    auto const trans = getTranslation(sk);
    start = trans.isRequestPersistentFailure()
      ? tc::ustubs().interpHelperNoTranslate
      : trans.addr();
  }

  vmJitReturnAddr() = nullptr;
  vmJitCalledFrame() = vmfp();
  SCOPE_EXIT { vmJitCalledFrame() = nullptr; };

  // If we can't get a translation at the current SrcKey, interpret basic
  // blocks until we end up somewhere with a translation (which we may have
  // created, if the lease holder dropped it).
  if (!start) {
    WorkloadStats guard(WorkloadStats::InInterp);

    tracing::BlockNoTrace _{"dispatch-bb"};

    while (!start) {
      INC_TPC(interp_bb);
      if (auto const retAddr = HPHP::dispatchBB()) {
        start = retAddr;
        break;
      }

      assertx(vmpc());
      sk = liveSK();

      auto const trans = getTranslation(sk);
      start = trans.isRequestPersistentFailure()
        ? tc::ustubs().interpHelperNoTranslate
        : trans.addr();
    }
  }

  if (Trace::moduleEnabled(Trace::ringbuffer, 1)) {
    auto skData = sk.valid() ? sk.toAtomicInt() : uint64_t(-1LL);
    Trace::ringbufferEntry(Trace::RBTypeResumeTC, skData, (uint64_t)start);
  }

  tl_regState = VMRegState::DIRTY;
  return start;
}

TCA handleResumeNoTranslate(bool interpFirst) {
  assert_native_stack_aligned();
  FTRACE(1, "handleResumeNoTranslate({})\n", interpFirst);

  if (!vmRegsUnsafe().pc) return tc::ustubs().callToExit;

  tl_regState = VMRegState::CLEAN;

  auto sk = liveSK();
  FTRACE(2, "handleResumeNoTranslate: sk: {}\n", showShort(sk));

  auto const find = [&] () -> TCA {
    if (auto const sr = tc::findSrcRec(sk)) {
      if (auto const tca = sr->getTopTranslation()) {
        if (LIKELY(RID().getJit())) {
          SKTRACE(2, sk, "handleResumeNoTranslate: found %p\n", tca);
          return tca;
        }
      }
    }
    return nullptr;
  };

  TCA start = nullptr;
  if (!interpFirst) start = find();

  vmJitReturnAddr() = nullptr;
  vmJitCalledFrame() = vmfp();
  SCOPE_EXIT { vmJitCalledFrame() = nullptr; };

  if (!start) {
    WorkloadStats guard(WorkloadStats::InInterp);
    tracing::BlockNoTrace _{"dispatch-bb"};
    do {
      INC_TPC(interp_bb);
      if (auto const retAddr = HPHP::dispatchBB()) {
        start = retAddr;
      } else {
        assertx(vmpc());
        sk = liveSK();
        start = find();
      }
    } while (!start);
  }

  if (Trace::moduleEnabled(Trace::ringbuffer, 1)) {
    auto skData = sk.valid() ? sk.toAtomicInt() : uint64_t(-1LL);
    Trace::ringbufferEntry(Trace::RBTypeResumeTC, skData, (uint64_t)start);
  }

  tl_regState = VMRegState::DIRTY;
  return start;
}

///////////////////////////////////////////////////////////////////////////////

}}}
