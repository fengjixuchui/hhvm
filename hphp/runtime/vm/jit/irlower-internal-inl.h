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

#include "hphp/runtime/base/array-data.h"
#include "hphp/runtime/base/attr.h"
#include "hphp/runtime/base/bespoke-array.h"
#include "hphp/runtime/base/datatype.h"
#include "hphp/runtime/base/header-kind.h"
#include "hphp/runtime/base/object-data.h"

#include "hphp/runtime/vm/jit/arg-group.h"
#include "hphp/runtime/vm/jit/code-gen-helpers.h"
#include "hphp/runtime/vm/jit/ir-instruction.h"
#include "hphp/runtime/vm/jit/ir-opcode.h"
#include "hphp/runtime/vm/jit/irlower.h"
#include "hphp/runtime/vm/jit/guard-type-profile.h"
#include "hphp/runtime/vm/jit/type.h"
#include "hphp/runtime/vm/jit/type-specialization.h"
#include "hphp/runtime/vm/jit/vasm.h"
#include "hphp/runtime/vm/jit/vasm-gen.h"
#include "hphp/runtime/vm/jit/vasm-instr.h"
#include "hphp/runtime/vm/jit/vasm-reg.h"

#include "hphp/util/asm-x64.h"

namespace HPHP { namespace jit { namespace irlower {

///////////////////////////////////////////////////////////////////////////////

inline Vout& vmain(IRLS& env) { assertx(env.vmain); return *env.vmain; }
inline Vout& vcold(IRLS& env) { assertx(env.vcold); return *env.vcold; }

inline Vlabel label(IRLS& env, Block* b) { return env.labels[b]; }

inline Vloc tmpLoc(IRLS& env, const SSATmp* tmp) {
  return env.locs[tmp];
}

inline Vloc srcLoc(IRLS& env, const IRInstruction* inst, unsigned i) {
  return tmpLoc(env, inst->src(i));
}

inline Vloc dstLoc(IRLS& env, const IRInstruction* inst, unsigned i) {
  return tmpLoc(env, inst->dst(i));
}

inline ArgGroup argGroup(IRLS& env, const IRInstruction* inst) {
  return ArgGroup(inst, env.locs);
}

inline CallDest callDest(Vreg reg0) {
  return { DestType::SSA, reg0 };
}

inline CallDest callDest(Vreg reg0, Vreg reg1) {
  return { DestType::TV, reg0, reg1 };
}

inline CallDest callDest(IRLS& env, const IRInstruction* inst) {
  if (inst->numDsts() == 0) return kVoidDest;
  assertx(inst->numDsts() == 1);

  auto const loc = dstLoc(env, inst, 0);
  DEBUG_ONLY auto const maybe_lval = inst->dst()->type().maybe(TLvalToCell);
  assertx(loc.numAllocated() == 1 || (maybe_lval && loc.numAllocated() == 2));
  assertx(IMPLIES(maybe_lval, inst->dst()->isA(TLvalToCell|TNullptr)));

  auto const dst = inst->dst();
  auto const kind = dst->isA(TBool) ? DestType::Byte :
                    dst->isA(TDbl) ? DestType::Dbl :
                    DestType::SSA;

  return { kind, dst->type(), loc.reg(0), loc.reg(1) };
}

inline CallDest callDestTV(IRLS& env, const IRInstruction* inst) {
  assertx(inst->numDsts() == 1);

  auto const loc = dstLoc(env, inst, 0);
  assertx(loc.numAllocated() == 1 || loc.numAllocated() == 2);

  if (loc.isFullSIMD()) {
    assertx(loc.numAllocated() == 1);
    return { DestType::SIMD, TCell, loc.reg(0) };
  }

  // loc.reg(1) may be InvalidReg, if the type is statically known. This is
  // expected and handled by users of CallDest.
  return { DestType::TV, TCell, loc.reg(0), loc.reg(1) };
}

///////////////////////////////////////////////////////////////////////////////

inline void fwdJcc(Vout& v, IRLS& env, ConditionCode cc,
                   Vreg sf, Vlabel target) {
  auto const next = v.makeBlock();
  v << jcc{cc, sf, {next, target}};
  v = next;
}

inline void fwdJcc(Vout& v, IRLS& env, ConditionCode cc,
                   Vreg sf, Block* target) {
  fwdJcc(v, env, cc, sf, label(env, target));
}

///////////////////////////////////////////////////////////////////////////////

namespace detail {

///////////////////////////////////////////////////////////////////////////////

/*
 * Materialize `data' into a Vreg and return it.
 */
inline Vreg materialize(Vout& v, Vptr data) {
  auto const t = v.makeReg();
  v << load{data, t};
  return t;
}
inline Vreg materialize(Vout&, Vreg data) { return data; }

template <class JmpFn>
void emitBespokeLayoutTest(Vout& v, ArrayLayout layout, Vreg r, JmpFn doJcc) {
  auto const check = layout.bespokeMaskAndCompare();
  auto const extra = BespokeArray::kExtraMagicBit.raw;
  const int16_t xorVal = static_cast<int16_t>(extra | check.xorVal);
  const int16_t andVal = static_cast<int16_t>(extra | check.andVal);
  const int16_t cmpVal = static_cast<int16_t>(check.cmpVal);

  auto const bits = v.makeReg();
  v << loadw{r[ArrayData::offsetOfBespokeIndex()], bits};

  auto const [cmpOp, sf] = [&] {
    auto const andUnnecessary = andVal == -1;
    auto const cmpUnnecessary = cmpVal == 0;

    if (andUnnecessary && cmpUnnecessary) {
      auto const sf = v.makeReg();
      v << cmpwi{xorVal, bits, sf};
      return std::make_pair(CC_Z, sf);
    }

    {
      // If our test can be reduced to a single bit test, do so.
      const int16_t andXor = andVal & xorVal;
      const int16_t andNotXor = andVal & ~xorVal;

      if (andNotXor == 0 && folly::popcount(andXor) == 1 &&
          (cmpVal == 0 || cmpVal == andXor)) {
        auto const sf = v.makeReg();
        v << testwi{andXor, bits, sf};
        return std::make_pair(cmpVal == 0 ? CC_NZ : CC_Z, sf);
      }
    }

    auto const xoredBits = v.makeReg();
    auto const xoredSf = v.makeReg();
    v << xorwi{xorVal, bits, xoredBits, xoredSf};

    auto const [maskedBits, maskedSf] = [&] {
      if (andUnnecessary) {
        // If we are not masking the result, and will not change the value or any
        // flags.
        return std::make_pair(xoredBits, xoredSf);
      }
      auto const res = v.makeReg();
      auto const resSf = v.makeReg();
      v << andwi{andVal, xoredBits, res, resSf};
      return std::make_pair(res, resSf);
    }();

    if (cmpUnnecessary) {
      // If we are comparing to 0, just use the zero flag from the result of the
      // previous ops.
      return std::make_pair(CC_Z, maskedSf);
    } else {
      auto const csf = v.makeReg();
      v << cmpwi{cmpVal, maskedBits, csf};
      return std::make_pair(CC_BE, csf);
    }
  }();

  doJcc(cmpOp, sf);
  auto const doneBlock = v.makeBlock();
  v << jmp{doneBlock};
  v = doneBlock;
}

/*
 * Test whether the value given by `dataSrc' has the same type specialization
 * as `type' does.
 *
 * Assumes that the DataType corresponding to `dataSrc' already matches `type'.
 */
template <class Loc, class JmpFn>
void emitSpecializedTypeTest(Vout& v, IRLS& /*env*/, Type type, Loc dataSrc,
                             JmpFn doJcc) {
  if (type < TRes) {
    // No cls field in Resource.
    always_assert(false && "unexpected guard on specialized Resource");
  }

  if (type == TStaticStr) {
    auto const sf = emitCmpRefCount(v, StaticValue, dataSrc);
    doJcc(CC_E, sf);
    return;
  }

  if (type < TObj || type < TCls) {
    // Emit the specific class test.
    assertx(type.clsSpec());
    assertx(type.clsSpec().exact() ||
            type.clsSpec().cls()->attrs() & AttrNoOverride);

    auto const data = materialize(v, dataSrc);
    auto const sf = v.makeReg();
    if (type < TObj) {
      emitCmpLowPtr(v, sf, type.clsSpec().cls(),
                    data[ObjectData::getVMClassOffset()]);
    } else {
      v << cmpq{v.cns(type.clsSpec().cls()), data, sf};
    }
    doJcc(CC_E, sf);
    return;
  }

  auto const spec = type.arrSpec();
  assertx(allowBespokeArrayLikes());
  assertx(!spec.type());

  auto const r = materialize(v, dataSrc);
  if (spec.vanilla()) {
    auto const sf = v.makeReg();
    v << testbim{ArrayData::kBespokeKindMask, r[HeaderKindOffset], sf};
    doJcc(CC_Z, sf);
  } else if (spec.bespoke()) {
    emitBespokeLayoutTest(v, spec.layout(), r, doJcc);
  } else {
    always_assert(false);
  }

}

///////////////////////////////////////////////////////////////////////////////

}

///////////////////////////////////////////////////////////////////////////////

template<class Loc, class JmpFn>
void emitTypeTest(Vout& v, IRLS& env, Type type,
                  Loc typeSrc, Loc dataSrc, Vreg sf, JmpFn doJcc) {
  // Note: If you add new supported type tests, you should update
  // negativeCheckType() to indicate whether it is precise or not.
  always_assert(!type.hasConstVal());
  always_assert_flog(
    !type.subtypeOfAny(TCountedStr, TPersistentArrLike),
    "Unsupported type in emitTypeTest(): {}", type
  );

  // Nothing to check.
  if (type == TCell) return;

  // Profile the type being guarded. We skip TUncounted here because that's
  // handled in emitIsTVTypeRefCounted, which has a number of other callers.
  if (RuntimeOption::EvalJitProfileGuardTypes && type != TUncounted) {
    emitProfileGuardType(v, type);
  }

  auto const cc = [&] {
    auto const cmp = [&] (DataType kind, ConditionCode cc) {
      emitCmpTVType(v, sf, kind, typeSrc);
      return cc;
    };

    auto const persistent_type = [&](DataType dt) {
      auto const masked = emitMaskTVType(v, ~kRefCountedBit, typeSrc);
      emitCmpTVType(v, sf, dt, masked);
      return CC_E;
    };

    // Type-tests of union types that may be specialized.
    auto const base = type.unspecialize();
    if (base == TVArr)      return persistent_type(KindOfPersistentVArray);
    if (base == TDArr)      return cmp(KindOfDArray, CC_LE);
    if (base == TVec)       return persistent_type(KindOfPersistentVec);
    if (base == TKeyset)    return persistent_type(KindOfPersistentKeyset);
    if (base == (TVArr|TDArr))  return cmp(KindOfVArray, CC_LE);
    if (base == TArrLike)   return cmp(KindOfKeyset, CC_LE);

    // Certain array-like type tests are more efficient post-HADVAs,
    // because dvarray types no longer exist when that flag is enabled.
    if (base == TDict) {
      return RO::EvalHackArrDVArrs ? cmp(KindOfDict, CC_LE)
                                   : persistent_type(KindOfPersistentDict);
    }
    if (base == (TVec|TDict)) {
      always_assert(RO::EvalHackArrDVArrs);
      return cmp(KindOfVec, CC_LE);
    }

    // Type-tests of union types that should not be specialized.
    if (type == TNull)      return cmp(KindOfNull, CC_BE);
    if (type == TStr)       return cmp(KindOfPersistentString, CC_AE);
    if (type == TStaticStr) return cmp(KindOfPersistentString, CC_AE);
    if (type == TUncountedInit) {
      auto const rtype = emitGetTVType(v, typeSrc);
      auto const sf2 = v.makeReg();
      emitTestTVType(v, sf2, kRefCountedBit, rtype);
      doJcc(CC_Z, sf2);

      static_assert(KindOfUninit == static_cast<DataType>(0),
                    "KindOfUninit == 0 in codegen");
      v << testb{rtype, rtype, sf};
      return CC_NZ;
    }
    if (type == TUncounted) {
      return ccNegate(emitIsTVTypeRefCounted(v, sf, typeSrc));
    }

    if (type == TInitCell) {
      auto const rtype = emitGetTVType(v, typeSrc);
      static_assert(KindOfUninit == static_cast<DataType>(0));
      v << testb{rtype, rtype, sf};
      return CC_NZ;
    }

    // All other valid types must not be unions.
    always_assert_flog(type.isKnownDataType(), "Unknown DataType: {}", type);
    always_assert_flog(!type.isUnion(), "Union type: {}", type);
    auto const dt = type.toDataType();
    return cmp(dt, CC_E);
  }();

  doJcc(cc, sf);

  if (type.isSpecialized() || type == TStaticStr) {
    detail::emitSpecializedTypeTest(v, env, type, dataSrc, doJcc);
  }
}

template<class Loc>
void emitTypeCheck(Vout& v, IRLS& env, Type type,
                   Loc typeSrc, Loc dataSrc, Block* taken) {
  emitTypeTest(v, env, type, typeSrc, dataSrc, v.makeReg(),
    [&] (ConditionCode cc, Vreg sf) {
      fwdJcc(v, env, ccNegate(cc), sf, taken);
    }
  );
}

///////////////////////////////////////////////////////////////////////////////

}}}
