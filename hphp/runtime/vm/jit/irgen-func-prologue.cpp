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

#include "hphp/runtime/vm/jit/irgen-func-prologue.h"

#include "hphp/runtime/base/array-iterator.h"
#include "hphp/runtime/base/attr.h"
#include "hphp/runtime/base/runtime-option.h"
#include "hphp/runtime/base/type-structure-helpers-defs.h"
#include "hphp/runtime/ext/asio/ext_resumable-wait-handle.h"
#include "hphp/runtime/vm/bytecode.h"
#include "hphp/runtime/vm/func.h"
#include "hphp/runtime/vm/hhbc.h"
#include "hphp/runtime/vm/reified-generics-info.h"
#include "hphp/runtime/vm/resumable.h"
#include "hphp/runtime/vm/srckey.h"

#include "hphp/runtime/vm/jit/extra-data.h"
#include "hphp/runtime/vm/jit/fixup.h"
#include "hphp/runtime/vm/jit/ir-opcode.h"
#include "hphp/runtime/vm/jit/ir-unit.h"
#include "hphp/runtime/vm/jit/irgen.h"
#include "hphp/runtime/vm/jit/irgen-exit.h"
#include "hphp/runtime/vm/jit/irgen-internal.h"
#include "hphp/runtime/vm/jit/irgen-state.h"
#include "hphp/runtime/vm/jit/prof-data.h"
#include "hphp/runtime/vm/jit/stack-offsets.h"
#include "hphp/runtime/vm/jit/translator.h"
#include "hphp/runtime/vm/jit/type.h"

#include "hphp/util/text-util.h"

namespace HPHP { namespace jit { namespace irgen {

///////////////////////////////////////////////////////////////////////////////

namespace {

/*
 * How to perform our stack overflow check.
 */
enum class StackCheck {
  None,   // not needed
  Early,  // must occur before setting up locals
  Combine // can be delayed and combined with surprise flags check
};

StackCheck stack_check_kind(const Func* func, uint32_t argc) {
  if (func->isPhpLeafFn() &&
      func->maxStackCells() < kStackCheckLeafPadding) {
    return StackCheck::None;
  }

  /*
   * Determine how many stack slots we're going to write that the caller hasn't
   * already checked we have space for.
   *
   * We don't need to worry about any of the passed parameter locals, because
   * the caller must have checked for that in its maxStackCells().  However,
   * we'd like to delay our stack overflow check until after we've entered our
   * frame, so we can combine it with the surprise flag check (which must run
   * after we've created the callee).
   *
   * The only things we are going to do is write uninits to the non-passed
   * params and to the non-parameter locals, and possibly shuffle some of the
   * locals into the variadic capture param.  The uninits are harmless to the
   * stack overflow code as long as we know we aren't going to segfault while
   * we write them.
   *
   * There's always sSurprisePageSize extra space at the bottom (lowest
   * addresses) of the eval stack, so we just only do this optimization if
   * we're sure we're going to write few enough uninits that we would be
   * staying within that region if the locals are actually too deep.
   */
  auto const safeFromSEGV = Stack::sSurprisePageSize / sizeof(TypedValue);

  return func->numLocals() < safeFromSEGV + argc
    ? StackCheck::Combine
    : StackCheck::Early;
}

} // namespace

///////////////////////////////////////////////////////////////////////////////

void emitCalleeGenericsChecks(IRGS& env, const Func* callee, SSATmp* callFlags,
                              bool pushed) {
  if (!callee->hasReifiedGenerics()) {
    // FIXME: leaks memory if generics were given but not expected nor pushed.
    if (pushed) {
      popDecRef(env);
      updateMarker(env);
      env.irb->exceptionStackBoundary();
    }
    return;
  }

  // Fail if generics were not passed.
  ifThenElse(
    env,
    [&] (Block* taken) {
      if (pushed) return;
      auto constexpr flag = 1 << CallFlags::Flags::HasGenerics;
      auto const hasGenerics = gen(env, AndInt, callFlags, cns(env, flag));
      gen(env, JmpZero, taken, hasGenerics);
    },
    [&] {
      // Generics were passed. Make them visible on the stack.
      auto const type = RuntimeOption::EvalHackArrDVArrs ? TVec : TVArr;
      auto const generics = pushed ? topC(env) : apparate(env, type);
      updateMarker(env);
      env.irb->exceptionStackBoundary();

      // Generics may be known if we are inlining.
      if (generics->hasConstVal(type)) {
        auto const genericsArr = generics->arrLikeVal();
        auto const& genericsDef =
          callee->getReifiedGenericsInfo().m_typeParamInfo;
        if (genericsArr->size() == genericsDef.size()) {
          bool match = true;
          IterateKV(genericsArr, [&](TypedValue k, TypedValue v) {
            assertx(tvIsInt(k) && tvIsArrayLike(v));
            auto const idx = k.m_data.num;
            auto const ts = v.m_data.parr;
            if (isWildCard(ts) && genericsDef[idx].m_isReified) {
              match = false;
              return true;
            }
            return false;
          });
          if (match) return;
        }
      }

      // Fail on generics count/wildcard mismatch.
      ifThen(
        env,
        [&] (Block* taken) {
          auto const match =
            gen(env, IsFunReifiedGenericsMatched, FuncData{callee}, callFlags);
          gen(env, JmpZero, taken, match);
        },
        [&] {
          hint(env, Block::Hint::Unlikely);
          gen(env, CheckFunReifiedGenericMismatch, cns(env, callee), generics);
        }
      );
    },
    [&] {
      hint(env, Block::Hint::Unlikely);

      // FIXME: ifThenElse() doesn't save/restore marker and stack boundary.
      updateMarker(env);
      env.irb->exceptionStackBoundary();

      if (pushed) {
        gen(env, Unreachable, ASSERT_REASON);
        return;
      }

      // Generics not given. We will either fail or raise a warning.
      if (!areAllGenericsSoft(callee->getReifiedGenericsInfo())) {
        gen(env, ThrowCallReifiedFunctionWithoutGenerics, cns(env, callee));
        return;
      }

      auto const errMsg = makeStaticString(folly::sformat(
        "Generic at index 0 to Function {} must be reified, erased given",
        callee->fullName()->data()));
      gen(env, RaiseWarning, cns(env, errMsg));

      // Push an empty array, as the remainder of the prologue assumes generics
      // are on the stack.
      arrprov::TagOverride ap_override{arrprov::tagFromSK(env.bcState)};
      push(env, cns(env, ArrayData::CreateVArray()));
      updateMarker(env);
      env.irb->exceptionStackBoundary();
    }
  );
}

namespace {

/*
 * Check for too few or too many arguments and trim extra args.
 */
void emitCalleeArgumentArityChecks(IRGS& env, const Func* callee,
                                   uint32_t argc) {
  if (argc < callee->numRequiredParams()) {
    gen(env, ThrowMissingArg, FuncArgData { callee, argc });
  }

  if (argc > callee->numParams()) {
    assertx(!callee->hasVariadicCaptureParam());
    assertx(argc == callee->numNonVariadicParams() + 1);

    // Pop unpack args, skipping generics (we already know their type).
    auto const generics = callee->hasReifiedGenerics()
      ? popC(env, DataTypeGeneric) : nullptr;
    auto const unpackArgs = pop(env, DataTypeGeneric);
    if (generics != nullptr) push(env, generics);

    // We have updated the stack.
    updateMarker(env);
    env.irb->exceptionStackBoundary();

    // Pass unpack args to the raiseTooManyArgumentsPrologue() helper, which
    // will use them to report the correct number and also take care of decref.
    auto const type = RuntimeOption::EvalHackArrDVArrs ? TVec : TVArr;
    auto const unpackArgsArr = gen(env, AssertType, type, unpackArgs);
    gen(env, RaiseTooManyArg, FuncData { callee }, unpackArgsArr);
  }
}

} // namespace

void emitCalleeDynamicCallChecks(IRGS& env, const Func* callee,
                                 SSATmp* callFlags) {
  if (!RuntimeOption::EvalNoticeOnBuiltinDynamicCalls || !callee->isBuiltin()) {
    return;
  }

  ifThen(
    env,
    [&] (Block* taken) {
      auto constexpr flag = 1 << CallFlags::Flags::IsDynamicCall;
      auto const isDynamicCall = gen(env, AndInt, callFlags, cns(env, flag));
      gen(env, JmpNZero, taken, isDynamicCall);
    },
    [&] {
      hint(env, Block::Hint::Unlikely);

      std::string errMsg;
      auto const fmtString = callee->isDynamicallyCallable()
        ? Strings::FUNCTION_CALLED_DYNAMICALLY_WITH_ATTRIBUTE
        : Strings::FUNCTION_CALLED_DYNAMICALLY_WITHOUT_ATTRIBUTE;
      string_printf(errMsg, fmtString, callee->fullName()->data());
      gen(env, RaiseNotice, cns(env, makeStaticString(errMsg)));
    }
  );
}

void emitCalleeCoeffectChecks(IRGS& env, const Func* callee,
                              SSATmp* callFlags) {
  assertx(callee);
  assertx(callFlags);

  if (!CoeffectsConfig::enabled()) return;
  auto const requiredCoeffects = callee->staticCoeffects().toRequired();

  if (callFlags->hasConstVal(TInt)) {
    auto const providedCoeffects =
      CallFlags(callFlags->intVal()).coeffects();
    if (LIKELY(providedCoeffects.canCall(requiredCoeffects))) return;
    gen(env, RaiseCoeffectsCallViolation, FuncData{callee}, fp(env), callFlags);
    return;
  }
  ifThen(
    env,
    [&] (Block* taken) {
      auto const providedCoeffects =
        gen(env, Lshr, callFlags, cns(env, CallFlags::CoeffectsStart));
      auto const cond =
        gen(env, GteInt, cns(env, requiredCoeffects.value()), providedCoeffects);
      gen(env, JmpZero, taken, cond);
    },
    [&] {
      hint(env, Block::Hint::Unlikely);
      gen(env, RaiseCoeffectsCallViolation, FuncData{callee}, fp(env), callFlags);
    }
  );
}

void emitCalleeImplicitContextChecks(IRGS& env, const Func* callee) {
  if (!RO::EvalEnableImplicitContext || !callee->hasNoContextAttr()) return;
  ifElse(
    env,
    [&] (Block* taken) {
      gen(env, CheckImplicitContextNull, taken);
    },
    [&] {
      hint(env, Block::Hint::Unlikely);
      auto const str = folly::to<std::string>(
        "Function ",
        callee->fullName()->data(),
        " has implicit context but is marked with __NoContext");
      auto const msg = cns(env, makeStaticString(str));
      gen(env, ThrowInvalidOperation, msg);
    }
  );
}

///////////////////////////////////////////////////////////////////////////////

namespace {

void emitPrologueEntry(IRGS& env, const Func* callee, uint32_t argc,
                       TransID transID) {
  gen(env, EnterPrologue);

  // Update marker with the stublogue bit.
  updateMarker(env);

  if (RuntimeOption::EvalHHIRGenerateAsserts) {
    // Make sure we are at the right function.
    auto const callFunc = gen(env, DefCallFunc);
    auto const callFuncOK = gen(env, EqFunc, callFunc, cns(env, callee));
    gen(env, JmpZero, makeUnreachable(env, ASSERT_REASON), callFuncOK);

    // Make sure we are at the right prologue.
    auto const numArgs = gen(env, DefCallNumArgs);
    auto const numArgsOK = gen(env, EqInt, numArgs, cns(env, argc));
    gen(env, JmpZero, makeUnreachable(env, ASSERT_REASON), numArgsOK);
  }

  // Emit debug code.
  if (Trace::moduleEnabled(Trace::ringbuffer)) {
    auto msg = RBMsgData { Trace::RBTypeFuncPrologue, callee->fullName() };
    gen(env, RBTraceMsg, msg);
  }

  // Increment profiling counter.
  if (isProfiling(env.context.kind)) {
    gen(env, IncProfCounter, TransIDData{transID});
    profData()->setProfiling(callee);
  }
}

void emitCalleeChecks(IRGS& env, const Func* callee, uint32_t argc,
                      SSATmp* callFlags) {
  // Generics are special and need to be checked first, as they may or may not
  // be on the stack. This check makes sure they materialize on the stack
  // if we expect them.
  emitCalleeGenericsChecks(env, callee, callFlags, false);
  emitCalleeArgumentArityChecks(env, callee, argc);
  emitCalleeDynamicCallChecks(env, callee, callFlags);
  emitCalleeCoeffectChecks(env, callee, callFlags);
  emitCalleeImplicitContextChecks(env, callee);

  // Emit early stack overflow check if necessary.
  if (stack_check_kind(callee, argc) == StackCheck::Early) {
    gen(env, CheckStackOverflow, sp(env));
  }
}

} // namespace

void emitInitFuncInputs(IRGS& env, const Func* callee, uint32_t argc) {
  // Reified generics were initialized by emitCalleeGenericsChecks().
  auto const generics = callee->hasReifiedGenerics()
    ? popC(env, DataTypeGeneric) : nullptr;

  // Push Uninit for un-passed arguments.
  auto const numParams = callee->numNonVariadicParams();
  while (argc < numParams) {
    push(env, cns(env, TUninit));
    ++argc;
  }

  if (argc < callee->numParams()) {
    // Push an empty array for `...$args'.
    arrprov::TagOverride _(RO::EvalArrayProvenance
      ? arrprov::Tag::Param(callee, numParams)
      : arrprov::Tag{});
    assertx(callee->hasVariadicCaptureParam());
    push(env, cns(env, ArrayData::CreateVArray()));
    ++argc;
  } else if (argc > callee->numParams()) {
    // Extra arguments already popped by emitCalleeArgumentArityChecks().
    assertx(!callee->hasVariadicCaptureParam());
    --argc;
  }

  assertx(argc == callee->numParams());

  // Place generics in the correct position.
  if (generics != nullptr) push(env, generics);
}

namespace {

void emitSpillFrame(IRGS& env, const Func* callee, uint32_t argc,
                    SSATmp* callFlags, SSATmp* prologueCtx) {
  auto const ctx = [&] {
    if (!callee->isClosureBody()) return prologueCtx;

    if (!callee->cls()) return cns(env, nullptr);
    if (callee->isStatic()) {
      return gen(env, LdClosureCls, Type::SubCls(callee->cls()), prologueCtx);
    }
    auto const closureThis =
      gen(env, LdClosureThis, Type::SubObj(callee->cls()), prologueCtx);
    gen(env, IncRef, closureThis);
    return closureThis;
  }();

  gen(env, DefFuncEntryFP, FuncData { callee },
      fp(env), sp(env), callFlags, ctx);
  auto const irSPOff = FPInvOffset { 0 };
  auto const bcSPOff = FPInvOffset { callee->numSlotsInFrame() };
  gen(env, DefFrameRelSP, DefStackData { irSPOff, bcSPOff }, fp(env));

  // We have updated stack and entered the context of the callee.
  updateMarker(env);
  env.irb->exceptionStackBoundary();

  // Increment the count for the latest call for optimized translations if we're
  // going to serialize the profile data.
  if (env.context.kind == TransKind::OptPrologue && isJitSerializing() &&
      RuntimeOption::EvalJitPGOOptCodeCallGraph) {
    gen(env, IncCallCounter, fp(env));
  }
}

} // namespace

/*
 * Set non-input locals to Uninit.
 */
void emitInitFuncLocals(IRGS& env, const Func* callee, SSATmp* prologueCtx) {
  /*
   * Maximum number of local initializations to unroll.
   *
   * The actual crossover point in terms of code size is 6 (just like for the
   * params init unroll limit); 9 was determined by experiment to be the
   * optimal point in certain benchmarks.
   *
   * FIXME: revisit this once these stores are elidable in the func body
   */
  constexpr auto kMaxLocalsInitUnroll = 9;

  // Parameters, generics and closure use variables are already initialized.
  auto numInited = callee->numParams();
  if (callee->hasReifiedGenerics()) ++numInited;

  // Push the closure's use variables (stored in closure object properties).
  if (callee->isClosureBody()) {
    auto const cls = callee->implCls();
    auto const numUses = cls->numDeclProperties();

    for (auto i = 0; i < numUses; ++i) {
      auto const ty =
        typeFromRAT(cls->declPropRepoAuthType(i), callee->cls()) & TCell;
      auto const addr = gen(env, LdPropAddr,
                            IndexData { cls->propSlotToIndex(i) },
                            ty.lval(Ptr::Prop), prologueCtx);
      auto const prop = gen(env, LdMem, ty, addr);
      gen(env, IncRef, prop);
      gen(env, StLoc, LocalId{numInited + i}, fp(env), prop);
    }

    decRef(env, prologueCtx);
    numInited += numUses;
  }

  auto const numLocals = callee->numLocals();
  assertx(numInited <= numLocals);

  // Set all remaining uninitialized locals to Uninit.
  if (numLocals - numInited <= kMaxLocalsInitUnroll) {
    for (auto i = numInited; i < numLocals; ++i) {
      gen(env, StLoc, LocalId{i}, fp(env), cns(env, TUninit));
    }
  } else {
    auto const range = LocalIdRange{numInited, (uint32_t)numLocals};
    gen(env, StLocRange, range, fp(env), cns(env, TUninit));
  }
}

namespace {

void emitJmpFuncBody(IRGS& env, const Func* callee, uint32_t argc) {
  // Check surprise flags in the same place as the interpreter: after setting
  // up the callee's frame but before executing any of its code.
  if (stack_check_kind(callee, argc) == StackCheck::Combine) {
    gen(env, CheckSurpriseAndStack, FuncEntryData { callee, argc }, fp(env));
  } else {
    gen(env, CheckSurpriseFlagsEnter, FuncEntryData { callee, argc }, fp(env));
  }

  // Emit the bindjmp for the function body.
  gen(
    env,
    ReqBindJmp,
    ReqBindJmpData {
      SrcKey { callee, callee->getEntryForNumArgs(argc), ResumeMode::None },
      FPInvOffset { callee->numSlotsInFrame() },
      spOffBCFromIRSP(env)
    },
    sp(env),
    fp(env)
  );
}

} // namespace

///////////////////////////////////////////////////////////////////////////////

namespace {

void definePrologueStack(IRGS& env, const Func* callee, uint32_t argc) {
  // The stack base of prologues points to the stack without the potentially
  // uninitialized space reserved for ActRec and inouts. The rvmsp() register
  // points to the future ActRec. The stack contains additional `argc' inputs
  // below the ActRec.
  auto const cells = callee->numInOutParamsForArgs(argc) + kNumActRecCells;
  auto const irSPOff = FPInvOffset { safe_cast<int32_t>(cells) };
  auto const bcSPOff = FPInvOffset { safe_cast<int32_t>(cells + argc) };
  gen(env, DefRegSP, DefStackData { irSPOff, bcSPOff });

  // Now that the stack is initialized, update the BC marker and perform
  // initial sync of the exception stack boundary.
  updateMarker(env);
  env.irb->exceptionStackBoundary();
}

Type prologueCtxType(const Func* func) {
  assertx(func->isClosureBody() || func->cls());
  if (func->isClosureBody()) return Type::ExactObj(func->implCls());
  if (func->isStatic()) return Type::SubCls(func->cls());
  return thisTypeFromFunc(func);
}

} // namespace

void emitFuncPrologue(IRGS& env, const Func* callee, uint32_t argc,
                      TransID transID) {
  assertx(argc <= callee->numNonVariadicParams() + 1);

  definePrologueStack(env, callee, argc);

  // Define register inputs before doing anything else that may clobber them.
  auto const callFlags = gen(env, DefCallFlags);
  auto const prologueCtx = (callee->isClosureBody() || callee->cls())
    ? gen(env, DefCallCtx, prologueCtxType(callee))
    : cns(env, nullptr);

  emitPrologueEntry(env, callee, argc, transID);
  emitCalleeChecks(env, callee, argc, callFlags);
  emitInitFuncInputs(env, callee, argc);
  emitSpillFrame(env, callee, argc, callFlags, prologueCtx);
  emitInitFuncLocals(env, callee, prologueCtx);
  emitJmpFuncBody(env, callee, argc);
}

///////////////////////////////////////////////////////////////////////////////

}}}
