<?hh
// Copyright (c) Facebook, Inc. and its affiliates. All Rights Reserved.

enum E : int {}

enum F : int {
  A = 0;
  use E;
}
