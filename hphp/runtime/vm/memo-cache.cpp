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

#include "memo-cache.h"

#include "hphp/runtime/base/string-data.h"
#include "hphp/runtime/base/tv-refcount.h"
#include "hphp/runtime/vm/runtime.h"

#include <bitset>
#include <type_traits>
#include <folly/container/F14Map.h>

namespace HPHP {

namespace memoCacheDetail {

// Dead simple hash combiner. This is a terrible hash function, but it serves to
// combine the two values and is very cheap (can be implemented with a single
// LEA on x64). We rely on F14's bit mixer to do the heavy work of scrambling
// the bits.
inline size_t combineHashes(uint64_t a, uint64_t b) {
  return (a * 9) + b;
}

////////////////////////////////////////////////////////////

/*
 * Keys in memo caches are composed of two pieces, the "header" and the
 * "storage". The header is what stores the meta-data for the key, which might
 * include the length of the key or the associated FuncId. In some cases, the
 * header might be empty, and it only serves as a policy class. The storage
 * stores the actual key values (which are represented as KeyElem), and might be
 * fixed-size, or a variable size data-structure. KeyElem doesn't know if it
 * stores a string or integer, so storage is responsible for tracking this
 * information as well. Everything is templated to allow for one implementation
 * of all the operations with keys which is efficient for each representation.
 */

// Represents a single element of a key (corresponding to a parameter).
struct KeyElem {
  union {
    int64_t i;
    StringData* s;
  };

  // These shouldn't be copied because we don't perform ref-count manipulations.
  KeyElem() = default;
  KeyElem(KeyElem&&) = delete;
  KeyElem(const KeyElem&) = delete;
  KeyElem& operator=(const KeyElem&) = delete;
  KeyElem& operator=(KeyElem&&) = delete;

  // Basic operations. KeyElem doesn't know whether its an integer or string, so
  // these operations must be told that.
  bool equals(const TypedValue& key, bool isString) const {
    if (!isString) return isIntType(key.m_type) && i == key.m_data.num;
    if (!isStringType(key.m_type)) return false;
    auto const s2 = key.m_data.pstr;
    return (s == s2) || s->same(s2);
  }

  bool equals(const KeyElem& other, bool isString) const {
    if (!isString) return i == other.i;
    return (s->hash() == other.s->hash()) && s->same(other.s);
  }

  size_t hash(bool isString) const {
    return isString ? s->hash() : i;
  }
};

////////////////////////////////////////////////////////////

/*
 * Headers. These represent the different ways we have of representing metadata
 * for a key.
 *
 * The operations they must support are:
 *  size()             - Return the number of keys
 *  equals()           - Check for equality against another header of the
 *                       same type
 *  startHash(1 param) - Combine the hash for this header with the other
 *                       hash value (used when computing a key's hash).
 *  startHash()        - Obtain the hash for this header (used as the initial
 *                       value when computing a key's hash)
 *  moved()            - This key is being moved away. Set the key count to 0
 *                       if the count isn't constant.
 */

// Non-shared, fixed size case. The header is empty and is just a policy
// class. The number of keys is a constant.
template <int N> struct EmptyHeader {
  size_t size() const { return N; }
  bool equals(EmptyHeader) const { return true; }
  size_t startHash(size_t firstHash) const {
    return firstHash;
  }
  // Always non-empty
  size_t startHash() const { always_assert(false); }
  void moved() {}
};
// Shared, fixed size case. The head just stores a FuncId (to distinguish
// different functions), but the number of keys is a constant.
template <int N> struct FuncIdHeader {
  explicit FuncIdHeader(FuncId funcId) : funcId{funcId} {}
  size_t size() const { return N; }
  bool equals(const FuncIdHeader& other) const {
    return funcId == other.funcId;
  }
  size_t startHash(size_t firstHash) const {
    return combineHashes(funcId, firstHash);
  }
  // Always non-empty
  size_t startHash() const { always_assert(false); }
  void moved() {}
  FuncId funcId;
};
// Generic case. Both the function and key count are stored (and are
// non-constant).
struct GenericHeader {
  explicit GenericHeader(GenericMemoId::Param id) : id{id} {}
  size_t size() const { return id.getKeyCount(); }
  bool equals(const GenericHeader& other) const {
    return id.asParam() == other.id.asParam();
  }
  size_t startHash(size_t firstHash) const {
    return combineHashes(id.asParam(), firstHash);
  }
  size_t startHash() const { return id.asParam(); }
  void moved() { id.setKeyCount(0); }
  GenericMemoId id;
};

////////////////////////////////////////////////////////////

// Fixed-size storage specialization. N is the number of keys, and H is the
// header to use. We derive from H to make use of the empty-base class
// optimization.
template <int N, typename H> struct FixedStorage : private H {
  static_assert(N > 0, "FixedStorage cannot be empty");
  using Header = H;

  explicit FixedStorage(Header header) : Header{header}
  { assertx(size() <= N); }

  FixedStorage(FixedStorage&& o) noexcept
    : Header{std::move(o)}
    , stringTags{std::move(o.stringTags)}
  {
    o.moved();
    o.stringTags.reset();
    for (size_t i = 0; i < size(); ++i) {
      elems[i].i = o.elems[i].i;
    }
  }

  FixedStorage(const FixedStorage&) = delete;
  FixedStorage& operator=(const FixedStorage&) = delete;
  FixedStorage& operator=(FixedStorage&&) = delete;

  size_t size() const { return this->Header::size(); }
  bool isString(size_t i) const { assertx(i < size()); return stringTags[i]; }
  void initIsString(size_t i) { assertx(i < size()); stringTags[i] = true; }
  void initIsInt(size_t) {}

  KeyElem& elem(size_t i) { assertx(i < size()); return elems[i]; }
  const KeyElem& elem(size_t i) const { assertx(i < size()); return elems[i]; }

  Header& header() { return *this; }
  const Header& header() const { return *this; }

  // If HasStringTags is true, we can do certain operations faster.
  static constexpr bool HasStringTags = true;
  bool compareStringTags(const FixedStorage& o) const {
    return stringTags == o.stringTags;
  }

  template <long unsigned int M>
  static std::bitset<N> castStringTags(const std::bitset<M>& o) {
    static_assert(M <= N, "");
    static_assert(N <= std::numeric_limits<unsigned long long>::digits, "");
    return std::bitset<N>{o.to_ullong()};
  }

  template <long unsigned int M>
  bool compareStringTags(const std::bitset<M>& o) const {
    return stringTags == castStringTags(o);
  }
  template <long unsigned int M>
  void setStringTags(const std::bitset<M>& o) {
    stringTags = castStringTags(o);
  }

  // The key elements are a fixed size, and we use a bitset to know which ones
  // are strings.
  std::array<KeyElem, N> elems;
  std::bitset<N> stringTags;
};

// Header specialization for a non-fixed number of keys. Used for generic memo
// caches.
struct UnboundStorage {
  // We always need a GenericHeader, so there's no need to parameterize it.
  using Header = GenericHeader;

  explicit UnboundStorage(Header header)
    : header_{header}
    , data{
      (header_.size() > 0)
        ? req::make_raw_array<Pair>(header_.size())
        : nullptr
      }
   {}

  UnboundStorage(UnboundStorage&& o) noexcept
    : header_{std::move(o.header_)}
    , data{o.data}
  {
    o.header_.moved();
    o.data = nullptr;
  }

  UnboundStorage(const UnboundStorage&) = delete;
  UnboundStorage& operator=(const UnboundStorage&) = delete;
  UnboundStorage& operator=(UnboundStorage&&) = delete;

  ~UnboundStorage() {
    if (data) req::destroy_raw_array(data, header_.size());
  }

  size_t size() const { return header_.size(); }
  bool isString(size_t i) const {
    assertx(i < size());
    return data[i].isString;
  }
  void initIsString(size_t i) { assertx(i < size()); data[i].isString = true; }
  void initIsInt(size_t i) { assertx(i < size()); data[i].isString = false; }

  KeyElem& elem(size_t i) { assertx(i < size()); return data[i].elem; }
  const KeyElem& elem(size_t i) const {
    assertx(i < size());
    return data[i].elem;
  }

  Header& header() { return header_; }
  const Header& header() const { return header_; }

  // We don't store the int/string markers for keys in a compacted bitset, so we
  // can't take advantage of some optimizations.
  static constexpr bool HasStringTags = false;
  template <typename T> bool compareStringTags(const T&) const {
    always_assert(false);
  }
  template <typename T> void setStringTags(const T&) {
    always_assert(false);
  }

  Header header_;
  // Use a dynamically allocated array of KeyElem and bool pairs to represent
  // the key.
  struct Pair {
    KeyElem elem;
    bool isString;
    TYPE_SCAN_CUSTOM() {
      if (isString) scanner.scan(elem.s);
    }
  };
  Pair* data;
};

////////////////////////////////////////////////////////////

// The actual key. The Key is instantiated on a particular kind of storage, and
// all the generic operations on it are implemented here.
template <typename S> struct Key {
  using Header = typename S::Header;

  template <typename P>
  Key(Header header, const P proxy)
    : storage{header}
  { proxy.initStorage(storage); }

  Key(Key&&) = default;
  Key(const Key&) = delete;
  Key& operator=(const Key&) = delete;
  Key& operator=(Key&&) = delete;

  ~Key() {
    // Dec-ref the strings
    for (size_t i = 0; i < storage.size(); ++i) {
      if (storage.isString(i)) storage.elem(i).s->decRefAndRelease();
    }
  }

  bool equals(const Key& o) const {
    // First compare the headers for equality
    if (!storage.header().equals(o.storage.header())) return false;
    // If the storage has string tags, we can compare all the types at once.
    if (S::HasStringTags && !storage.compareStringTags(o.storage)) {
      return false;
    }
    for (size_t i = 0; i < storage.size(); ++i) {
      // If the storage doesn't have string tags, we need to compare the type
      // one at a time.
      if (!S::HasStringTags && storage.isString(i) != o.storage.isString(i)) {
        return false;
      }
      if (!storage.elem(i).equals(o.storage.elem(i), storage.isString(i))) {
        return false;
      }
    }
    return true;
  }

  template <typename P>
  bool equals(Header header, P proxy) const {
    if (UNLIKELY(!storage.header().equals(header))) return false;
    return proxy.equals(storage);
  }

  size_t hash() const {
    // If the key has no elements (which can happen in generic caches), just use
    // the hash for the header.
    if (storage.size() <= 0) return storage.header().startHash();
    // Otherwise, combine the hash for the first element and the header.
    auto hash = storage.header().startHash(
      storage.elem(0).hash(storage.isString(0))
    );
    // And then combine it with the rest of the hashes for the other key
    // elements.
    for (size_t i = 1; i < storage.size(); ++i) {
      hash = combineHashes(
        hash,
        storage.elem(i).hash(storage.isString(i))
      );
    }
    return hash;
  }

  TYPE_SCAN_CUSTOM() {
    for (size_t i = 0, n = storage.size(); i < n; ++i) {
      if (storage.isString(i)) scanner.scan(storage.elem(i).s);
    }
  }

  S storage;
};

// Instantiations for the supported possibilities.
template <int N> using FixedKey = Key<FixedStorage<N, EmptyHeader<N>>>;
template <int N> using FixedFuncIdKey = Key<FixedStorage<N, FuncIdHeader<N>>>;
using UnboundKey = Key<UnboundStorage>;

////////////////////////////////////////////////////////////

// KeySource is an abstraction for obtaining TypedValues from either
// locals or the stack.

struct FrameKeySource {
  const ActRec* fp;
  uint64_t begin;
  TypedValue operator[](size_t idx) const {
    assertx(begin + idx < fp->func()->numLocals());
    return *frame_local(fp, begin + idx);
  }
};
struct StackKeySource {
  const TypedValue* keys;
  // NB: We index backwards here to keep the same order as indexing
  // into locals.
  TypedValue operator[](size_t idx) const { return keys[-idx]; }
};

////////////////////////////////////////////////////////////

/*
 * KeyProxy is a wrapper around the pointer to the TypedValue array
 * passed into the get/set function. It allows us to do lookups in the
 * memo cache without having to move or transform those Cells. It
 * comes in two flavors: KeyProxy, where the key types are not known
 * and must be checked at runtme, and KeyProxyWithTypes, where the key
 * types are known statically.
 */
template <typename KeySource>
struct KeyProxy {
  KeySource keys;

  template <typename H>
  size_t hash(H header) const {
    // If there's no key elements (which can happen with generic memo-caches),
    // just use the hash from the header.
    if (header.size() <= 0) return header.startHash();
    auto const getHash = [](const TypedValue& c) {
      assertx(tvIsPlausible(c));
      assertx(isIntType(c.m_type) || isStringType(c.m_type));
      return isIntType(c.m_type) ? c.m_data.num : c.m_data.pstr->hash();
    };
    // Otherwise, combine the hash from the header and the first element, and
    // then combine that with the rest of the element hashes.
    auto hash = header.startHash(getHash(keys[0]));
    for (size_t i = 1; i < header.size(); ++i) {
      hash = combineHashes(hash, getHash(keys[i]));
    }
    return hash;
  }

  template <typename S>
  bool equals(const S& storage) const {
    for (size_t i = 0; i < storage.size(); ++i) {
      auto const tv = keys[i];
      assertx(tvIsPlausible(tv));
      assertx(isIntType(tv.m_type) || isStringType(tv.m_type));
      if (UNLIKELY(!storage.elem(i).equals(tv, storage.isString(i)))) {
        return false;
      }
    }
    return true;
  }

  template <typename S>
  void initStorage(S& storage) const {
    // Given a storage, initialize it with values from this KeyProxy. Used when
    // we're storing a Key and need to turn a KeyProxy into a Key.
    for (size_t i = 0; i < storage.size(); ++i) {
      auto const tv = keys[i];
      assertx(tvIsPlausible(tv));
      assertx(isIntType(tv.m_type) || isStringType(tv.m_type));
      if (isStringType(tv.m_type)) {
        tv.m_data.pstr->incRefCount();
        storage.initIsString(i);
      } else {
        storage.initIsInt(i);
      }
      storage.elem(i).i = tv.m_data.num;
    }
  }
};

// Key types and count are known statically, so we can use compile-time
// recursion to implement most operations.
template <typename KeySource, bool IsStr, bool... Rest>
struct KeyProxyWithTypes {
  static constexpr auto Size = sizeof...(Rest)+1;

  using Next = KeyProxyWithTypes<KeySource, Rest...>;

  KeySource keys;

  template <typename H>
  size_t hash(H header) const {
    assertx(header.size() == Size);
    return Next{keys}.template hashRec<1>(header.startHash(hashAt<0>()));
  }

  template <typename S>
  bool equals(const S& storage) const {
    assertx(storage.size() == Size);
    if (S::HasStringTags &&
        UNLIKELY(!storage.compareStringTags(makeBitset()))) {
      return false;
    }
    return equalsRec<0>(storage);
  }

  template <typename S>
  void initStorage(S& storage) const {
    assertx(storage.size() == Size);
    if (S::HasStringTags) storage.setStringTags(makeBitset());
    initStorageRec<0>(storage);
  }

  template <int N> size_t hashRec(size_t hash) const {
    return Next{keys}.template hashRec<N+1>(combineHashes(hash, hashAt<N>()));
  }

  template <int N, typename S>
  bool equalsRec(const S& storage) const {
    auto const tv = keys[N];
    assertx(tvIsPlausible(tv));
    assertx(!IsStr || isStringType(tv.m_type));
    assertx(IsStr || isIntType(tv.m_type));
    if (!S::HasStringTags && UNLIKELY(storage.isString(N) != IsStr)) {
      return false;
    }
    assertx(!S::HasStringTags || storage.isString(N) == IsStr);
    if (IsStr) {
      auto const s = tv.m_data.pstr;
      auto const s2 = storage.elem(N).s;
      if (UNLIKELY(s != s2 && !s->same(s2))) return false;
    } else if (UNLIKELY(storage.elem(N).i != tv.m_data.num)) {
      return false;
    }
    return Next{keys}.template equalsRec<N+1>(storage);
  }

  template <int N, typename S>
  void initStorageRec(S& storage) const {
    auto const tv = keys[N];
    assertx(tvIsPlausible(tv));
    assertx(!IsStr || isStringType(tv.m_type));
    assertx(IsStr || isIntType(tv.m_type));
    if (IsStr) {
      if (!S::HasStringTags) storage.initIsString(N);
      tv.m_data.pstr->incRefCount();
    } else if (!S::HasStringTags) {
      storage.initIsInt(N);
    }
    storage.elem(N).i = tv.m_data.num;
    Next{keys}.template initStorageRec<N+1>(storage);
  }

  template <int N> size_t hashAt() const {
    auto const tv = keys[N];
    assertx(tvIsPlausible(tv));
    assertx(!IsStr || isStringType(tv.m_type));
    assertx(IsStr || isIntType(tv.m_type));
    return IsStr ? tv.m_data.pstr->hash() : tv.m_data.num;
  };

  static constexpr std::bitset<Size> makeBitset() {
    std::bitset<Size> b;
    makeBitsetRec<0, Size>(b);
    return b;
  }

  template <int M, int N>
  static constexpr void makeBitsetRec(std::bitset<N>& b) {
    b[M] = IsStr;
    Next::template makeBitsetRec<M+1, N>(b);
  }
};

// Base case for recursion. KeyProxy with one element.
template <typename KeySource, bool IsStr>
struct KeyProxyWithTypes<KeySource, IsStr> {
  KeySource keys;

  template <typename H>
  size_t hash(H header) const {
    assertx(header.size() == 1);
    return header.startHash(hashAt<0>());
  }

  template <typename S>
  bool equals(const S& storage) const {
    assertx(storage.size() == 1);
    if (S::HasStringTags &&
        UNLIKELY(!storage.compareStringTags(makeBitset()))) {
      return false;
    }
    return equalsRec<0>(storage);
  }

  template <typename S>
  void initStorage(S& storage) const {
    assertx(storage.size() == 1);
    if (S::HasStringTags) storage.setStringTags(makeBitset());
    initStorageRec<0>(storage);
  }

  template <int N> size_t hashRec(size_t hash) const {
    return combineHashes(hash, hashAt<N>());
  }

  template <int N, typename S>
  bool equalsRec(const S& storage) const {
    auto const tv = keys[N];
    assertx(tvIsPlausible(tv));
    assertx(!IsStr || isStringType(tv.m_type));
    assertx(IsStr || isIntType(tv.m_type));
    if (!S::HasStringTags && UNLIKELY(storage.isString(N) != IsStr)) {
      return false;
    }
    assertx(!S::HasStringTags || storage.isString(N) == IsStr);
    if (IsStr) {
      auto const s = tv.m_data.pstr;
      auto const s2 = storage.elem(N).s;
      return s == s2 || s->same(s2);
    }
    return storage.elem(N).i == keys[N].m_data.num;
  }

  template <int N, typename S>
  void initStorageRec(S& storage) const {
    auto const tv = keys[N];
    assertx(tvIsPlausible(tv));
    assertx(!IsStr || isStringType(tv.m_type));
    assertx(IsStr || isIntType(tv.m_type));
    if (IsStr) {
      if (!S::HasStringTags) storage.initIsString(N);
      tv.m_data.pstr->incRefCount();
    } else if (!S::HasStringTags) {
      storage.initIsInt(N);
    }
    storage.elem(N).i = tv.m_data.num;
  }

  template <int N> size_t hashAt() const {
    auto const tv = keys[N];
    assertx(tvIsPlausible(tv));
    assertx(!IsStr || isStringType(tv.m_type));
    assertx(IsStr || isIntType(tv.m_type));
    return IsStr ? tv.m_data.pstr->hash() : tv.m_data.num;
  };

  static constexpr std::bitset<1> makeBitset() {
    std::bitset<1> b;
    makeBitsetRec<0, 1>(b);
    return b;
  }

  template <int M, int N>
  static constexpr void makeBitsetRec(std::bitset<N>& b) { b[M] = IsStr; }
};

////////////////////////////////////////////////////////////

// Equality and hasher functions. We mark them as "transparent" to allow for
// mixed-type lookups.

template <typename K> struct KeyEquals {
  using is_transparent = void;

  template <typename P>
  bool operator()(std::tuple<typename K::Header, P> k1,
                  const K& k2) const {
    return LIKELY(k2.equals(std::get<0>(k1), std::get<1>(k1)));
  }
  bool operator()(const K& k1, const K& k2) const {
    return k1.equals(k2);
  }
};

template <typename K> struct KeyHasher {
  using is_transparent = void;

  template <typename P>
  size_t operator()(std::tuple<typename K::Header, P> k) const {
    return std::get<1>(k).hash(std::get<0>(k));
  }
  size_t operator()(const K& k) const { return k.hash(); }
};

// The SharedOnlyKey has already been hashed, so its an identity here.
struct SharedOnlyKeyHasher {
  size_t operator()(SharedOnlyKey k) const { return k; }
};

////////////////////////////////////////////////////////////

// Wrapper around a TypedValue to handle the ref-count manipulations for us.
struct TVWrapper {
  explicit TVWrapper(TypedValue value) : value{value}
  { tvIncRefGen(value); }
  TVWrapper(TVWrapper&& o) noexcept: value{o.value} {
    o.value = make_tv<KindOfNull>();
  }
  TVWrapper(const TVWrapper&) = delete;
  TVWrapper& operator=(const TVWrapper&) = delete;
  TVWrapper& operator=(TVWrapper&& o) noexcept {
    std::swap(value, o.value);
    return *this;
  }
  ~TVWrapper() { tvDecRefGen(value); }
  TypedValue value;
};

////////////////////////////////////////////////////////////

}

}

namespace folly {

// Mark the SharedOnlyKeyHasher as avalanching so that F14 doesn't use the bit
// mixer on it.
template <typename K>
struct IsAvalanchingHasher<HPHP::memoCacheDetail::SharedOnlyKeyHasher, K>
  : std::true_type {};

}

namespace HPHP {

namespace memoCacheDetail {

////////////////////////////////////////////////////////////

// For the specialized and generic caches
template <typename K> struct MemoCache : MemoCacheBase {
  using Cache = folly::F14ValueMap<
    K,
    TVWrapper,
    KeyHasher<K>,
    KeyEquals<K>,
    req::ConservativeAllocator<std::pair<const K, TVWrapper>>
  >;
  Cache cache;

  MemoCache() = default;
  MemoCache(const MemoCache&) = delete;
  MemoCache(MemoCache&&) = delete;
  MemoCache& operator=(const MemoCache&) = delete;
  MemoCache& operator=(MemoCache&&) = delete;

  TYPE_SCAN_CUSTOM() {
    using value_type = typename Cache::value_type; // pair
    cache.visitContiguousRanges(
      [&](value_type const* start, value_type const* end) {
        scanner.scan(*start, (const char*)end - (const char*)start);
      });
  }
};

// For the shared-only caches (which do not need any of the key machinery).
struct SharedOnlyMemoCache : MemoCacheBase {
  using Cache = folly::F14ValueMap<
    SharedOnlyKey,
    TVWrapper,
    SharedOnlyKeyHasher,
    std::equal_to<SharedOnlyKey>,
    req::ConservativeAllocator<std::pair<const SharedOnlyKey, TVWrapper>>
  >;
  Cache cache;

  SharedOnlyMemoCache() = default;
  SharedOnlyMemoCache(const SharedOnlyMemoCache&) = delete;
  SharedOnlyMemoCache(SharedOnlyMemoCache&&) = delete;
  SharedOnlyMemoCache& operator=(const SharedOnlyMemoCache&) = delete;
  SharedOnlyMemoCache& operator=(SharedOnlyMemoCache&&) = delete;

  TYPE_SCAN_CUSTOM() {
    using value_type = typename Cache::value_type; // pair
    cache.visitContiguousRanges(
      [&](value_type const* start, value_type const* end) {
        scanner.scan(*start, (const char*)end - (const char*)start);
      });
  }
};

}

////////////////////////////////////////////////////////////

using namespace memoCacheDetail;

namespace {

// Helper functions. Given a pointer to a MemoCacheBase, return a pointer to the
// appropriate F14 cache embedded within it. If we're in debug builds, we'll use
// dynamic-cast for this to catch places where someone might be mixing different
// cache types on the same pointer. Otherwise, we'll just use static cast.

template <typename C>
ALWAYS_INLINE
auto const& getCache(const MemoCacheBase* base) {
  if (debug) {
    auto ptr = dynamic_cast<const C*>(base);
    always_assert(ptr != nullptr);
    return ptr->cache;
  }
  return static_cast<const C*>(base)->cache;
}


template <typename C>
ALWAYS_INLINE
auto& getCache(MemoCacheBase* base) {
  if (debug) {
    auto ptr = dynamic_cast<C*>(base);
    always_assert(ptr != nullptr);
    return ptr->cache;
  }
  return static_cast<C*>(base)->cache;
}

// The get and set logic for all the different cache types is the same, so
// factor it out into these helper functions.

template <typename K, typename P>
ALWAYS_INLINE
const TypedValue* getImpl(const MemoCacheBase* base,
                          typename K::Header header,
                          P keys) {
  auto const& cache = getCache<MemoCache<K>>(base);
  auto const it = cache.find(std::make_tuple(header, keys));
  if (it == cache.end()) return nullptr;
  assertx(tvIsPlausible(it->second.value));
  assertx(it->second.value.m_type != KindOfUninit);
  return &it->second.value;
}

template <typename K, typename P>
ALWAYS_INLINE
void setImpl(MemoCacheBase*& base,
             typename K::Header header,
             P keys,
             TypedValue val) {
  assertx(tvIsPlausible(val));
  assertx(val.m_type != KindOfUninit);
  if (!base) base = req::make_raw<MemoCache<K>>();
  auto& cache = getCache<MemoCache<K>>(base);
  cache.insert_or_assign(K{header, keys}, TVWrapper{val});
}

}

////////////////////////////////////////////////////////////

// Getter and setter implementations. These all just delegate to the above
// getter/setter helpers, instantiated on the appropriate key and key-proxy
// type.

#define KEY_PROXY_WITH_TYPES(Source) KeyProxyWithTypes<Source, IsStr...>
#define KEY_PROXY_NO_TYPES(Source) KeyProxy<Source>

#define FIXED_KEY_TYPES FixedKey<sizeof...(IsStr)>
#define FIXED_KEY_TYPES_HEADER typename FixedKey<sizeof...(IsStr)>::Header
#define FIXED_KEY_TYPES_TEMPL template<bool... IsStr>

#define FIXED_KEY_NO_TYPES FixedKey<N>
#define FIXED_KEY_NO_TYPES_HEADER typename FixedKey<N>::Header
#define FIXED_KEY_NO_TYPES_TEMPL template <int N>

#define FIXED_KEY_TYPES_FUNC FixedFuncIdKey<sizeof...(IsStr)>
#define FIXED_KEY_TYPES_FUNC_HEADER \
  typename FixedFuncIdKey<sizeof...(IsStr)>::Header
#define FIXED_KEY_TYPES_FUNC_TEMPL template<bool... IsStr>

#define FIXED_KEY_NO_TYPES_FUNC FixedFuncIdKey<N>
#define FIXED_KEY_NO_TYPES_FUNC_HEADER typename FixedFuncIdKey<N>::Header
#define FIXED_KEY_NO_TYPES_FUNC_TEMPL template <int N>

#define UNBOUND_KEY UnboundKey
#define UNBOUND_KEY_HEADER UnboundKey::Header
#define UNBOUND_KEY_TEMPL

#define FP_PARAMS const ActRec* fp, uint64_t begin
#define FP_TYPE FrameKeySource
#define FP_PROXY_PARAMS FrameKeySource{fp, begin}

#define SP_PARAMS const TypedValue* keys
#define SP_TYPE StackKeySource
#define SP_PROXY_PARAMS StackKeySource{keys}

#define NO_EXTRA_PARAMS
#define NO_EXTRA_HEADER_PARAMS

#define EXTRA_FUNC_ID_PARAMS FuncId funcId,
#define EXTRA_FUNC_ID_HEADER_PARAMS funcId

#define EXTRA_MEMO_ID_PARAMS GenericMemoId::Param id,
#define EXTRA_MEMO_ID_HEADER_PARAMS id

#define STATIC static
#define NO_STATIC

#define O(Name, Source, SourceName, Extra, KeyType, Proxy, Static)      \
KeyType##_TEMPL                                                         \
Static const TypedValue* Name##Get##SourceName(const MemoCacheBase* base, \
                                               Extra##_PARAMS           \
                                               Source##_PARAMS) {       \
  return getImpl<KeyType>(                                              \
    base,                                                               \
    KeyType##_HEADER{Extra##_HEADER_PARAMS},                            \
    Proxy(Source##_TYPE){Source##_PROXY_PARAMS}                         \
  );                                                                    \
}                                                                       \
KeyType##_TEMPL                                                         \
Static void Name##Set##SourceName(MemoCacheBase*& base,                 \
                                  Extra##_PARAMS                        \
                                  Source##_PARAMS,                      \
                                  TypedValue val) {                     \
  setImpl<KeyType>(                                                     \
    base,                                                               \
    KeyType##_HEADER{Extra##_HEADER_PARAMS},                            \
    Proxy(Source##_TYPE){Source##_PROXY_PARAMS},                        \
    val                                                                 \
  );                                                                    \
}

O(memoCache, FP, FP, NO_EXTRA, FIXED_KEY_TYPES, KEY_PROXY_WITH_TYPES, STATIC);
O(memoCache, SP, SP, NO_EXTRA, FIXED_KEY_TYPES, KEY_PROXY_WITH_TYPES, STATIC);

O(memoCacheShared, FP, FP, EXTRA_FUNC_ID, FIXED_KEY_TYPES_FUNC,
  KEY_PROXY_WITH_TYPES, STATIC);
O(memoCacheShared, SP, SP, EXTRA_FUNC_ID, FIXED_KEY_TYPES_FUNC,
  KEY_PROXY_WITH_TYPES, STATIC);

O(memoCacheGenericKeys, FP, FP, NO_EXTRA, FIXED_KEY_NO_TYPES,
  KEY_PROXY_NO_TYPES, STATIC);
O(memoCacheGenericKeys, SP, SP, NO_EXTRA, FIXED_KEY_NO_TYPES,
  KEY_PROXY_NO_TYPES, STATIC);

O(memoCacheSharedGenericKeys, FP, FP, EXTRA_FUNC_ID, FIXED_KEY_NO_TYPES_FUNC,
  KEY_PROXY_NO_TYPES, STATIC);
O(memoCacheSharedGenericKeys, SP, SP, EXTRA_FUNC_ID, FIXED_KEY_NO_TYPES_FUNC,
  KEY_PROXY_NO_TYPES, STATIC);

O(memoCacheGeneric, FP, FP, EXTRA_MEMO_ID, UNBOUND_KEY,
  KEY_PROXY_NO_TYPES, NO_STATIC);
O(memoCacheGeneric, SP, SP, EXTRA_MEMO_ID, UNBOUND_KEY,
  KEY_PROXY_NO_TYPES, NO_STATIC);

#undef O

const TypedValue* memoCacheGetSharedOnly(const MemoCacheBase* base,
                                         SharedOnlyKey key) {
  auto const& cache = getCache<SharedOnlyMemoCache>(base);
  auto const it = cache.find(key);
  if (it == cache.end()) return nullptr;
  assertx(tvIsPlausible(it->second.value));
  assertx(it->second.value.m_type != KindOfUninit);
  return &it->second.value;
}

void memoCacheSetSharedOnly(MemoCacheBase*& base,
                            SharedOnlyKey key,
                            TypedValue val) {
  assertx(tvIsPlausible(val));
  assertx(val.m_type != KindOfUninit);
  if (!base) base = req::make_raw<SharedOnlyMemoCache>();
  auto& cache = getCache<SharedOnlyMemoCache>(base);
  cache.insert_or_assign(key, TVWrapper{val});
}

////////////////////////////////////////////////////////////

namespace {

// We'll generate specialized getters and setters for every possible combination
// of key types up to this limit. This causes an exponential blow up in
// functions (and in compile-time) so we want to be careful about increasing it.
static constexpr size_t kMemoCacheMaxSpecializedKeyTypes = 4;
static_assert(
  kMemoCacheMaxSpecializedKeyTypes <= kMemoCacheMaxSpecializedKeys,
  ""
);

// Use a macro to generate a "builder" class, which is responsible for taking
// key types and key counts, and returning the appropriate getter or setter.

#define O(Type, Name, Func1, Func2)                                     \
template <int, typename = void> struct Name##Builder;                   \
template <int M>                                                        \
struct Name##Builder<M,                                                 \
                     std::enable_if_t<                                  \
                       (M <= kMemoCacheMaxSpecializedKeyTypes)>> {      \
  template <int N, bool... IsStr>                                       \
  struct FromTypes {                                                    \
    static Type get(const bool* types, size_t count) {                  \
      if (types[count - N]) {                                           \
        return FromTypes<N-1, IsStr..., true>::get(types, count);       \
      } else {                                                          \
        return FromTypes<N-1, IsStr..., false>::get(types, count);      \
      }                                                                 \
    };                                                                  \
  };                                                                    \
                                                                        \
  template <bool... IsStr>                                              \
  struct FromTypes<0, IsStr...> {                                       \
    static Type get(const bool*, size_t) {                              \
      return Func1<IsStr...>;                                           \
    }                                                                   \
  };                                                                    \
                                                                        \
  static Type get(const bool* types, size_t count) {                    \
    if (count == M) return FromTypes<M>::get(types, count);             \
    return Name##Builder<M-1>::get(types, count);                       \
  }                                                                     \
  static Type get(size_t count) {                                       \
    if (count == M) return Func2<M>;                                    \
    return Name##Builder<M-1>::get(count);                              \
  }                                                                     \
};                                                                      \
template <int M>                                                        \
struct Name##Builder<M,                                                 \
                     std::enable_if_t<                                  \
                       (M > kMemoCacheMaxSpecializedKeyTypes)>> {       \
  static Type get(const bool* types, size_t count) {                    \
    if (count == M) return Func2<M>;                                    \
    return Name##Builder<M-1>::get(types, count);                       \
  }                                                                     \
  static Type get(size_t count) {                                       \
    if (count == M) return Func2<M>;                                    \
    return Name##Builder<M-1>::get(count);                              \
  }                                                                     \
};                                                                      \
template <> struct Name##Builder<0> {                                   \
  static Type get(const bool*, size_t) { return nullptr; }              \
  static Type get(size_t) { return nullptr; }                           \
};

#define O2(TypeG, TypeGS, TypeS, TypeSS, Suffix)                     \
O(TypeG, MemoCacheGet##Suffix,                                       \
  memoCacheGet##Suffix, memoCacheGenericKeysGet##Suffix)             \
O(TypeGS, MemoCacheGetShared##Suffix,                                \
  memoCacheSharedGet##Suffix, memoCacheSharedGenericKeysGet##Suffix) \
O(TypeS, MemoCacheSet##Suffix,                                       \
  memoCacheSet##Suffix, memoCacheGenericKeysSet##Suffix)             \
O(TypeSS, MemoCacheSetShared##Suffix,                                \
  memoCacheSharedSet##Suffix, memoCacheSharedGenericKeysSet##Suffix)

O2(MemoCacheGetterFP, SharedMemoCacheGetterFP, MemoCacheSetterFP,
   SharedMemoCacheSetterFP, FP);
O2(MemoCacheGetterSP, SharedMemoCacheGetterSP, MemoCacheSetterSP,
   SharedMemoCacheSetterSP, SP);

#undef O2
#undef O

}

#define O(Ret, Type, Builder)                                           \
Ret Type##ForKeyTypesFP(const bool* types,                              \
                        const Func* func,                               \
                        size_t count) {                                 \
  return Builder##FPBuilder<kMemoCacheMaxSpecializedKeys>::get(types, count); \
}                                                                       \
Ret Type##ForKeyCountFP(const Func* func, size_t count) { \
  return Builder##FPBuilder<kMemoCacheMaxSpecializedKeys>::get(count); \
}
O(MemoCacheGetterFP, memoCacheGet, MemoCacheGet);
O(MemoCacheSetterFP, memoCacheSet, MemoCacheSet);
O(SharedMemoCacheGetterFP, sharedMemoCacheGet, MemoCacheGetShared);
O(SharedMemoCacheSetterFP, sharedMemoCacheSet, MemoCacheSetShared);
#undef O

#define O(Ret, Type, Builder)                                           \
Ret Type##ForKeyTypesSP(const bool* types, size_t count) {              \
  return Builder##SPBuilder<kMemoCacheMaxSpecializedKeys>::get(types, count); \
}                                                                       \
Ret Type##ForKeyCountSP(size_t count) {                                 \
  return Builder##SPBuilder<kMemoCacheMaxSpecializedKeys>::get(count);  \
}
O(MemoCacheGetterSP, memoCacheGet, MemoCacheGet);
O(MemoCacheSetterSP, memoCacheSet, MemoCacheSet);
O(SharedMemoCacheGetterSP, sharedMemoCacheGet, MemoCacheGetShared);
O(SharedMemoCacheSetterSP, sharedMemoCacheSet, MemoCacheSetShared);
#undef O

}
