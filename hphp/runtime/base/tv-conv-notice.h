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

#pragma once

#include "hphp/runtime/base/type-string.h"

namespace HPHP {

struct StringData;

enum class ConvNoticeLevel: uint8_t { None, Log, Throw };

const char* convOpToName(ConvNoticeLevel level);

template <typename T> ConvNoticeLevel flagToConvNoticeLevel(T flag) {
  return static_cast<ConvNoticeLevel>(
     flag + static_cast<uint8_t>(ConvNoticeLevel::None)
  );
}

void handleConvNoticeLevel(
   ConvNoticeLevel Level,
   const char* const from,
   const char* const to,
   const StringData* reason);

extern const StaticString s_ConvNoticeReasonConcat;
extern const StaticString s_ConvNoticeReasonBitOp;
extern const StaticString s_ConvNoticeReasonIncDec;
extern const StaticString s_ConvNoticeReasonMath;

}
