/**
 * Autogenerated by Thrift
 *
 * DO NOT EDIT UNLESS YOU ARE SURE THAT YOU KNOW WHAT YOU ARE DOING
 *  @generated
 */
#pragma once

#include <thrift/lib/cpp2/gen/module_data_h.h>

#include "thrift/lib/thrift/gen-cpp2/reflection_types.h"

namespace apache { namespace thrift { namespace reflection {

struct _TypeEnumDataStorage {
  using type = Type;
  static constexpr const std::size_t size = 16;
  static constexpr const std::array<Type, 16> values = {{
    Type::TYPE_VOID,
    Type::TYPE_STRING,
    Type::TYPE_BOOL,
    Type::TYPE_BYTE,
    Type::TYPE_I16,
    Type::TYPE_I32,
    Type::TYPE_I64,
    Type::TYPE_DOUBLE,
    Type::TYPE_ENUM,
    Type::TYPE_LIST,
    Type::TYPE_SET,
    Type::TYPE_MAP,
    Type::TYPE_STRUCT,
    Type::TYPE_SERVICE,
    Type::TYPE_PROGRAM,
    Type::TYPE_FLOAT,
  }};
  static constexpr const std::array<folly::StringPiece, 16> names = {{
    "TYPE_VOID",
    "TYPE_STRING",
    "TYPE_BOOL",
    "TYPE_BYTE",
    "TYPE_I16",
    "TYPE_I32",
    "TYPE_I64",
    "TYPE_DOUBLE",
    "TYPE_ENUM",
    "TYPE_LIST",
    "TYPE_SET",
    "TYPE_MAP",
    "TYPE_STRUCT",
    "TYPE_SERVICE",
    "TYPE_PROGRAM",
    "TYPE_FLOAT",
  }};
};

}}} // apache::thrift::reflection

namespace apache { namespace thrift {

template <> struct TEnumDataStorage<::apache::thrift::reflection::Type> {
  using storage_type = ::apache::thrift::reflection::_TypeEnumDataStorage;
};

}} // apache::thrift
