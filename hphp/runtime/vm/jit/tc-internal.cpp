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

#include "hphp/runtime/vm/jit/tc-internal.h"
#include "hphp/runtime/vm/jit/tc.h"

#include "hphp/runtime/base/init-fini-node.h"
#include "hphp/runtime/base/perf-warning.h"
#include "hphp/runtime/base/runtime-option.h"
#include "hphp/runtime/base/stats.h"
#include "hphp/runtime/vm/debug/debug.h"
#include "hphp/runtime/vm/vm-regs.h"
#include "hphp/runtime/vm/workload-stats.h"

#include "hphp/runtime/vm/jit/code-cache.h"
#include "hphp/runtime/vm/jit/guard-type-profile.h"
#include "hphp/runtime/vm/jit/mcgen-translate.h"
#include "hphp/runtime/vm/jit/mcgen.h"
#include "hphp/runtime/vm/jit/perf-counters.h"
#include "hphp/runtime/vm/jit/prof-data.h"
#include "hphp/runtime/vm/jit/relocation.h"
#include "hphp/runtime/vm/jit/srcdb.h"
#include "hphp/runtime/vm/jit/stub-alloc.h"
#include "hphp/runtime/vm/jit/tc-prologue.h"
#include "hphp/runtime/vm/jit/tc-record.h"
#include "hphp/runtime/vm/jit/timer.h"
#include "hphp/runtime/vm/jit/trans-db.h"
#include "hphp/runtime/vm/jit/unique-stubs.h"
#include "hphp/runtime/vm/jit/unwind-itanium.h"
#include "hphp/runtime/vm/jit/vasm-emit.h"
#include "hphp/runtime/vm/jit/write-lease.h"

#include "hphp/util/disasm.h"
#include "hphp/util/mutex.h"
#include "hphp/util/rds-local.h"
#include "hphp/util/trace.h"

#include <tbb/concurrent_hash_map.h>

#include <atomic>

extern "C" _Unwind_Reason_Code
__gxx_personality_v0(int, _Unwind_Action, uint64_t, _Unwind_Exception*,
                     _Unwind_Context*);

TRACE_SET_MOD(mcg);

namespace HPHP { namespace jit { namespace tc {

CodeCache* g_code{nullptr};
SrcDB g_srcDB;
UniqueStubs g_ustubs;

///////////////////////////////////////////////////////////////////////////////

namespace {

std::atomic<uint64_t> s_numTrans;
SimpleMutex s_codeLock{false, RankCodeCache};
SimpleMutex s_metadataLock{false, RankCodeMetadata};
RDS_LOCAL_NO_CHECK(size_t, s_initialTCSize);

bool shouldPGOFunc(const Func* func) {
  return profData() != nullptr;
}

// A code reuse block owns temporary code blocks used to emit into a reused
// segment of another view.
struct CodeReuseBlock {
  CodeBlock reusedMain, reusedCold, reusedFrozen;

  // Get a view into possibly reused code blocks (if there is space, and
  // reusable TC is enabled).
  CodeCache::View getMaybeReusedView(CodeCache::View& src,
                                     const TransRange& range) {
    if (!RuntimeOption::EvalEnableReusableTC) return src;
    auto main = &src.main();
    auto cold = &src.cold();
    auto frozen = &src.frozen();

    auto const pad = RuntimeOption::EvalReusableTCPadding;
    size_t mainSize   = range.main.size() + pad;
    size_t coldSize   = range.cold.size() + pad;
    size_t frozenSize = range.frozen.size() + pad;
    if (auto const s = (TCA)main->allocInner(mainSize)) {
      reusedMain.init(s, mainSize, "Reused main");
      main = &reusedMain;
    }
    if (auto const s = (TCA)cold->allocInner(coldSize)) {
      reusedCold.init(s, coldSize, "Reused cold");
      cold = &reusedCold;
    }
    if (cold != frozen) {
      if (auto const s = (TCA)frozen->allocInner(frozenSize)) {
        reusedFrozen.init(s, frozenSize, "Reused frozen");
        frozen = &reusedFrozen;
      }
    }

    return CodeCache::View(*main, *cold, *frozen, src.data(), false);
  }
};

}
///////////////////////////////////////////////////////////////////////////////

TransLoc TransRange::loc() const {
  TransLoc loc;
  loc.setMainStart(main.begin());
  loc.setColdStart(cold.begin() - sizeof(uint32_t));
  loc.setFrozenStart(frozen.begin() - sizeof(uint32_t));
  loc.setMainSize(main.size());

  assertx(loc.coldCodeSize() == cold.size());
  assertx(loc.frozenCodeSize() == frozen.size());
  return loc;
}

bool canTranslate() {
  return s_numTrans.load(std::memory_order_relaxed) <
    RuntimeOption::EvalJitGlobalTranslationLimit;
}

using FuncCounterMap = tbb::concurrent_hash_map<FuncId, uint32_t,
                                                FuncIdHashCompare>;
static FuncCounterMap s_func_counters;

using SrcKeyCounters = tbb::concurrent_hash_map<SrcKey, uint32_t,
                                                SrcKey::TbbHashCompare>;

static SrcKeyCounters s_sk_counters;

bool shouldTranslateNoSizeLimit(SrcKey sk, TransKind kind) {
  // If we've hit Eval.JitGlobalTranslationLimit, then we stop translating.
  if (!canTranslate()) {
    return false;
  }

  auto const func = sk.func();

  // Do not translate functions from units marked as interpret-only.
  if (func->unit()->isInterpretOnly()) {
    return false;
  }

  // Refuse to JIT Live translations if Eval.JitPGOOnly is enabled.
  if (RuntimeOption::EvalJitPGOOnly &&
      (kind == TransKind::Live || kind == TransKind::LivePrologue)) {
    return false;
  }

  // Refuse to JIT Live / Profile translations for a function until
  // Eval.JitLiveThreshold / Eval.JitProfileThreshold is hit.
  const bool isLive = kind == TransKind::Live ||
                      kind == TransKind::LivePrologue;
  const bool isProf = kind == TransKind::Profile ||
                      kind == TransKind::ProfPrologue;
  if (isLive || isProf) {
    uint32_t skCount = 1;
    if (RuntimeOption::EvalJitSrcKeyThreshold > 1) {
      SrcKeyCounters::accessor acc;
      if (!s_sk_counters.insert(acc, SrcKeyCounters::value_type(sk, 1))) {
        skCount = ++acc->second;
      }
    }
    {
      FuncCounterMap::accessor acc;
      if (!s_func_counters.insert(acc, {func->getFuncId(), 1})) ++acc->second;
      auto const funcThreshold = isLive ? RuntimeOption::EvalJitLiveThreshold
                                        : RuntimeOption::EvalJitProfileThreshold;
      if (acc->second < funcThreshold) return false;
    }
    if (skCount < RuntimeOption::EvalJitSrcKeyThreshold) return false;
  }

  return true;
}

static std::atomic_flag s_did_log = ATOMIC_FLAG_INIT;
static std::atomic<bool> s_TCisFull{false};

bool shouldTranslate(SrcKey sk, TransKind kind) {
  if (s_TCisFull.load(std::memory_order_relaxed) ||
      !shouldTranslateNoSizeLimit(sk, kind)) {
    return false;
  }

  const auto serverMode = RuntimeOption::ServerExecutionMode();
  const auto maxTransTime = RuntimeOption::EvalJitMaxRequestTranslationTime;
  const auto transCounter = Timer::CounterValue(Timer::mcg_translate);

  if (serverMode && maxTransTime >= 0 &&
      transCounter.wall_time_elapsed >= maxTransTime) {

    if (Trace::moduleEnabledRelease(Trace::mcg, 1)) {
      Trace::traceRelease("Skipping translation. "
                          "Time budget of %" PRId64 " exceeded. "
                          "%" PRId64 "us elapsed. "
                          "%" PRId64 " translations completed\n",
                          maxTransTime,
                          transCounter.wall_time_elapsed,
                          transCounter.count);
    }
    return false;
  }

  auto const main_under = code().main().used() < CodeCache::AMaxUsage;
  auto const cold_under = code().cold().used() < CodeCache::AColdMaxUsage;
  auto const froz_under = code().frozen().used() < CodeCache::AFrozenMaxUsage;

  // Otherwise, follow the Eval.JitAMaxUsage limits.
  if (main_under && cold_under && froz_under) return true;

  // We use cold and frozen for all kinds of translations, but we allow PGO
  // translations past the limit for main if there's still space in code.hot.
  if (cold_under && froz_under) {
    switch (kind) {
      case TransKind::ProfPrologue:
      case TransKind::Profile:
      case TransKind::OptPrologue:
      case TransKind::Optimize:
        return code().hotEnabled();
      default:
        break;
    }
  }

  // Set a flag so we quickly bail from trying to generate new translations next
  // time.
  s_TCisFull.store(true, std::memory_order_relaxed);
  Treadmill::enqueue([] { s_sk_counters.clear(); });

  if (main_under && !s_did_log.test_and_set() &&
      RuntimeOption::EvalProfBranchSampleFreq == 0) {
    // If we ran out of TC space in cold or frozen but not in main, something
    // unexpected is happening and we should take note of it.  We skip this
    // logging if TC branch profiling is on, since it fills up code and frozen
    // at a much higher rate.
    if (!cold_under) {
      logPerfWarning("cold_full", 1, [] (StructuredLogEntry&) {});
    }
    if (!froz_under) {
      logPerfWarning("frozen_full", 1, [] (StructuredLogEntry&) {});
    }
  }
  return false;
}

bool newTranslation() {
  if (s_numTrans.fetch_add(1, std::memory_order_relaxed) >=
      RuntimeOption::EvalJitGlobalTranslationLimit) {
    return false;
  }
  return true;
}

std::unique_lock<SimpleMutex> lockCode(bool lock) {
  if (lock) return std::unique_lock<SimpleMutex>{ s_codeLock };
  return std::unique_lock<SimpleMutex>{s_codeLock, std::defer_lock};
}

std::unique_lock<SimpleMutex> lockMetadata(bool lock) {
  if (lock) return std::unique_lock<SimpleMutex>{s_metadataLock};
  return std::unique_lock<SimpleMutex>{s_metadataLock, std::defer_lock};
}

CodeMetaLock::CodeMetaLock(bool f) :
    m_code(lockCode(f)),
    m_meta(lockMetadata(f)) {
}

void CodeMetaLock::lock() {
  m_code.lock();
  m_meta.lock();
}

void CodeMetaLock::unlock() {
  m_meta.unlock();
  m_code.unlock();
}

void assertOwnsCodeLock(OptView v) {
  if (!v || !v->isLocal()) s_codeLock.assertOwnedBySelf();
}
void assertOwnsMetadataLock() { s_metadataLock.assertOwnedBySelf(); }

void requestInit() {
  tl_regState = VMRegState::CLEAN;
  Timer::RequestInit();
  memset(rl_perf_counters.getCheck(), 0, sizeof(PerfCounters));
  Stats::init();
  requestInitProfData();
  *s_initialTCSize.getCheck() = g_code->totalUsed();
  assertx(!g_unwind_rds.isInit());
  memset(g_unwind_rds.get(), 0, sizeof(UnwindRDS));
  g_unwind_rds.markInit();
}

void requestExit() {
  Stats::dump();
  Stats::clear();
  if (RuntimeOption::EvalJitProfileGuardTypes) {
    logGuardProfileData();
  }
  Timer::RequestExit();
  if (profData()) profData()->maybeResetCounters();
  requestExitProfData();

  reportJitMaturity();

  if (Trace::moduleEnabledRelease(Trace::mcgstats, 1)) {
    Trace::traceRelease("MCGenerator perf counters for %s:\n",
                        g_context->getRequestUrl(50).c_str());
    for (int i = 0; i < tpc_num_counters; i++) {
      Trace::traceRelease("%-20s %10" PRId64 "\n",
                          kPerfCounterNames[i], rl_perf_counters[i]);
    }
    Trace::traceRelease("\n");
  }
}

void codeEmittedThisRequest(size_t& requestEntry, size_t& now) {
  requestEntry = *s_initialTCSize;
  now = g_code->totalUsed();
}

void processInit() {
  auto codeLock = lockCode();
  auto metaLock = lockMetadata();

  g_code = new(low_malloc(sizeof(CodeCache))) CodeCache();
  g_ustubs.emitAll(*g_code, *Debug::DebugInfo::Get());

  // Write an .eh_frame section that covers the JIT portion of the TC.
  initUnwinder(g_code->base(), g_code->tcSize(),
               tc_unwind_personality);

  if (auto cti_cap = g_code->bytecode().capacity()) {
    // write an .eh_frame for cti code using default personality
    initUnwinder(g_code->bytecode().base(), cti_cap, __gxx_personality_v0);
  }

  Disasm::ExcludedAddressRange(g_code->base(), g_code->codeSize());

  recycleInit();
}

void processExit() {
  recycleStop();
}

bool isValidCodeAddress(TCA addr) {
  return g_code->isValidCodeAddress(addr);
}

bool isProfileCodeAddress(TCA addr) {
  return g_code->prof().contains(addr);
}

bool isHotCodeAddress(TCA addr) {
  return g_code->hot().contains(addr);
}

void freeTCStub(TCA stub) {
  // We need to lock the code because s_freeStubs.push() writes to the stub and
  // the metadata to protect s_freeStubs itself.
  auto codeLock = lockCode();
  auto metaLock = lockMetadata();

  assertx(code().frozen().contains(stub));

  markStubFreed(stub);
}

void checkFreeProfData() {
  // In PGO mode, we free all the profiling data once the main code area reaches
  // its maximum usage and either the hot area is also full or all the functions
  // that were profiled have already been optimized.
  //
  // However, we keep the data around indefinitely in a few special modes:
  // * Eval.EnableReusableTC
  // * TC dumping enabled (Eval.DumpTC/DumpIR/etc.)
  //
  // Finally, when the RetranslateAll mode is enabled, the ProfData is discarded
  // via a different mechanism, after all the optimized translations are
  // generated.
  if (profData() &&
      !RuntimeOption::EvalEnableReusableTC &&
      code().main().used() >= CodeCache::AMaxUsage &&
      (!code().hotEnabled() ||
       profData()->profilingFuncs() == profData()->optimizedFuncs()) &&
      !transdb::enabled() &&
      !mcgen::retranslateAllEnabled()) {
    discardProfData();
  }
}

static void dropSrcDBProfIncomingBranches() {
  auto const base     = code().prof().base();
  auto const frontier = code().prof().frontier();
  for (auto& it : srcDB()) {
    auto sr = it.second;
    sr->removeIncomingBranchesInRange(base, frontier);
  }
}

void freeProfCode() {
  Treadmill::enqueue([]{
    dropSrcDBProfIncomingBranches();
    code().freeProf();
    // Clearing the inline stacks map is purely an optimization, and it barely
    // buys us anything when we're using jumpstart (because we have very few
    // profiling translations, if any), so we skip it in this case.
    if (!isJitDeserializing()) {
      auto metaLock = lockMetadata();
      auto const base     = code().prof().base();
      auto const frontier = code().prof().frontier();
      eraseInlineStacksInRange(base, frontier);
    }
  });
}

bool shouldProfileNewFuncs() {
  if (profData() == nullptr) return false;

  // We have two knobs to control the number of functions we're allowed to
  // profile: Eval.JitProfileRequests and Eval.JitProfileBCSize. We profile new
  // functions until either of these limits is exceeded. In practice, we expect
  // to hit the bytecode size limit first, but we keep the request limit around
  // as a safety net.
  return profData()->profilingBCSize() < RuntimeOption::EvalJitProfileBCSize &&
    requestCount() < RuntimeOption::EvalJitProfileRequests;
}

bool profileFunc(const Func* func) {
  // If retranslateAll has been scheduled (including cases when it is going on,
  // or has finished), we can't emit more Profile translations.  This is to
  // ensure that, when retranslateAll() runs, no more Profile translations are
  // being added to ProfData.
  if (mcgen::retranslateAllScheduled()) return false;

  if (code().prof().used() >= CodeCache::AProfMaxUsage) return false;

  if (!shouldPGOFunc(func)) return false;

  if (profData()->optimized(func->getFuncId())) return false;

  // If we already started profiling `func', then we return true and skip the
  // other checks below.
  if (profData()->profiling(func->getFuncId())) return true;

  return shouldProfileNewFuncs();
}

///////////////////////////////////////////////////////////////////////////////

LocalTCBuffer::LocalTCBuffer(Address start, size_t initialSize) {
  TCA fakeStart = code().threadLocalStart();
  auto const sz = initialSize / 4;
  auto initBlock = [&] (DataBlock& block, size_t mxSz, const char* nm) {
    always_assert(sz <= mxSz);
    block.init(fakeStart, start, sz, mxSz, nm);
    fakeStart += mxSz;
    start += sz;
  };
  initBlock(m_main, RuntimeOption::EvalThreadTCMainBufferSize,
            "thread local main");
  initBlock(m_cold, RuntimeOption::EvalThreadTCColdBufferSize,
            "thread local cold");
  initBlock(m_frozen, RuntimeOption::EvalThreadTCFrozenBufferSize,
            "thread local frozen");
  initBlock(m_data, RuntimeOption::EvalThreadTCDataBufferSize,
            "thread local data");
}

OptView LocalTCBuffer::view() {
  if (!valid()) return folly::none;
  return CodeCache::View(m_main, m_cold, m_frozen, m_data, true);
}

////////////////////////////////////////////////////////////////////////////////
// Translator internals
Translator::Translator(SrcKey sk, TransKind kind)
  : sk(sk), kind(kind), unit(), vunit()
{}

Translator::~Translator() = default;

folly::Optional<TCA> Translator::acquireLeaseAndRequisitePaperwork() {
  computeKind();

  // Avoid a race where we would create a Live translation while
  // retranslateAll is in flight and we haven't generated an
  // Optimized translation yet.
  auto const shouldEmitLiveTranslation = [&] {
    if (mcgen::retranslateAllPending() && !isProfiling(kind) && profData()) {
      // Functions that are marked as being profiled or marked as having been
      // optimized are about to have their translations invalidated during the
      // publish phase of retranslate all.  Don't allow live translations to be
      // emitted in this scenario.
      auto const fid = sk.func()->getFuncId();
      return !profData()->profiling(fid) &&
             !profData()->optimized(fid);
    }
    return true;
  };
  if (!shouldEmitLiveTranslation()) {
    return nullptr;
  }

  if (auto const p = getCached()) return *p;

  // Acquire the appropriate lease otherwise bail to a fallback
  // execution mode (eg. interpreter) by returning a nullptr
  // translation address.
  m_lease.emplace(sk.func(), kind);
  if (!(*m_lease)) return nullptr;
  computeKind();  // Recompute the kind in case we are no longer profiling.
  if (!m_lease->checkKind(kind)) return nullptr;

  // Check again if we can emit live translations for the given
  // func now that we have the lock.
  if (!shouldEmitLiveTranslation()) {
    return nullptr;
  }

  if (!shouldTranslate()) return nullptr;

  if (UNLIKELY(RID().isJittingDisabled())) {
    TRACE(2, "punting because jitting code was disabled\n");
    return nullptr;
  }

  // Check for cached one last time since we have all the locks now.
  return getCached();
}

bool Translator::shouldTranslate(bool noSizeLimit) {
  if (kind == TransKind::Invalid) computeKind();
  if (noSizeLimit) {
    return shouldTranslateNoSizeLimit(sk, kind);
  }
  return ::HPHP::jit::tc::shouldTranslate(sk, kind);
}

void Translator::translate(folly::Optional<CodeCache::View> view) {
  if (isProfiling(kind)) {
    transId = profData()->allocTransID();
  }

  if (!newTranslation()) return;

  WorkloadStats::EnsureInit();
  WorkloadStats guard(WorkloadStats::InTrans);
  SCOPE_EXIT {
    unit.reset();
    vunit.reset();
  };
  this->gen();

  // Check for translation failure.
  if (!vunit) return;

  Timer timer(Timer::mcg_finishTranslation);

  tracing::Block _b{
    "emit-translation",
      [&] {
        return traceProps(*vunit);
      }
  };

  auto codeLock = lockCode(false);
  if (!view.hasValue()) {
    if (RuntimeOption::EvalEnableReusableTC) {
      auto const initialSize = 256;
      m_localBuffer = std::make_unique<uint8_t[]>(initialSize);
      m_localTCBuffer =
        std::make_unique<LocalTCBuffer>(m_localBuffer.get(), initialSize);
      view = m_localTCBuffer->view();
    } else {
      // Using the global TC view.  Better lock things.
      codeLock.lock();
    }
  }

  // Tag the translation start, and build the trans meta.
  // Generate vasm into the code view, retrying if we fill hot.
  while (true) {
    if (!view.hasValue() || !view->isLocal()) {
      view.emplace(code().view(kind));
    }
    CGMeta fixups;
    TransLocMaker maker{*view};
    maker.markStart();
    try {
      emitVunit(*vunit, unit.get(), *view, fixups,
                mcgen::dumpTCAnnotation(kind) ? getAnnotations()
                                              : nullptr);
    } catch (const DataBlockFull& dbFull) {
      always_assert(!view->isLocal());
      if (dbFull.name == "hot") {
        code().disableHot();
        // Rollback tags and try again.
        maker.rollback();
        fixups.clear();
        continue;
      }
      auto const range = maker.markEnd();
      auto const bytes = range.main.size() + range.cold.size() +
                         range.frozen.size();
      // There should be few of these.  They mean there is wasted work
      // performing translation for functions that don't have space in the TC.
      logPerfWarning("translation_overflow", 1, [&] (StructuredLogEntry& e) {
        e.setStr("kind", show(kind));
        e.setStr("srckey", show(sk));
        e.setStr("data_block", dbFull.name);
        e.setInt("bytes_dropped", bytes);
      });
      reset();
      return;
    }
    transMeta.emplace(*view);
    transMeta->fixups = std::move(fixups);
    transMeta->range = maker.markEnd();
    break;
  }

  if (isProfiling(kind)) {
    profData()->setProfiling(sk.func());
  }

  Timer metaTimer(Timer::mcg_finishTranslation_metadata);
  if (unit && unit->logEntry()) {
    auto metaLock = lockMetadata();
    logTranslation(this, transMeta->range);
  }

  if (!RuntimeOption::EvalJitLogAllInlineRegions.empty()) {
    logFrames(*vunit);
  }
}

bool Translator::translateSuccess() const {
  return transMeta.has_value();
}

void Translator::relocate() {
  assertx(transMeta.hasValue());
  // Code emitted directly is relocated during emission (or emitted
  // directly in place).
  if (!transMeta->view.isLocal()) {
    assertx(!RuntimeOption::EvalEnableReusableTC);
    return;
  }

  WorkloadStats::EnsureInit();
  WorkloadStats guard(WorkloadStats::InTrans);

  auto const& range = transMeta->range;
  auto& fixups = transMeta->fixups;

  RelocationInfo rel;
  {
    auto codeLock = lockCode();
    while (true) {
      auto finalView = code().view(kind);
      CodeReuseBlock crb;
      auto dstView = crb.getMaybeReusedView(finalView, range);
      auto& srcView = transMeta->view;
      TransLocMaker maker{dstView};
      maker.markStart();

      try {
        auto origin = range.data;
        if (!origin.empty()) {
          dstView.data().bytes(origin.size(),
                               srcView.data().toDestAddress(origin.begin()));

          auto dest = maker.dataRange();
          auto oAddr = origin.begin();
          auto dAddr = dest.begin();
          while (oAddr != origin.end()) {
            assertx(dAddr != dest.end());
            rel.recordAddress(oAddr++, dAddr++, 0);
          }
        }

        jit::relocate(rel, dstView.main(), range.main.begin(), range.main.end(),
                      srcView.main(), fixups, nullptr, AreaIndex::Main);
        jit::relocate(rel, dstView.cold(), range.cold.begin(), range.cold.end(),
                      srcView.cold(), fixups, nullptr, AreaIndex::Cold);
        if (&srcView.cold() != &srcView.frozen()) {
          jit::relocate(rel, dstView.frozen(), range.frozen.begin(),
                        range.frozen.end(), srcView.frozen(), fixups, nullptr,
                        AreaIndex::Frozen);
        }

      } catch (const DataBlockFull& dbFull) {
        if (dbFull.name == "hot") {
          maker.rollback();
          code().disableHot();
          continue;
        }
        auto const bytes = range.main.size() + range.cold.size() +
                           range.frozen.size();
        // There should be few of these.  They mean there is wasted work
        // performing translation for functions that don't have space in the TC.
        logPerfWarning("translation_overflow", 1, [&] (StructuredLogEntry& e) {
          e.setStr("kind", show(kind));
          e.setStr("srckey", show(sk));
          e.setStr("data_block", dbFull.name);
          e.setInt("bytes_dropped", bytes);
        });
        reset();
        return;
      }
      transMeta->range = maker.markEnd();
      transMeta->view = finalView;
      break;
    }
  }
  adjustForRelocation(rel);
  adjustMetaDataForRelocation(rel, nullptr, fixups);
  adjustCodeForRelocation(rel, fixups);
}

TCA Translator::publish() {
  assertx(transMeta.hasValue());
  auto codeLock = lockCode();
  auto metaLock = lockMetadata();
  publishMetaInternal();
  publishCodeInternal();
  return transMeta->range.loc().entry();
}

void Translator::publishMetaInternal() {
  assertx(transMeta.hasValue());
  this->publishMetaImpl();
}

void Translator::publishCodeInternal() {
  assertx(transMeta.hasValue());
  this->publishCodeImpl();
  updateCodeSizeCounters();
}

////////////////////////////////////////////////////////////////////////////////

}}}
