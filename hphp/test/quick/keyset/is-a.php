<?hh
// Copyright 2004-present Facebook. All Rights Reserved.

class Foo {}

function test_is_a($a, $interfaces) {
  echo "====================================================\n";
  echo "Testing: ";
  var_dump($a);

  echo "\tgettype: ";
  var_dump(gettype($a));

  echo "\tis_null: ";
  var_dump(is_null($a));

  echo "\tis_bool: ";
  var_dump(is_bool($a));

  echo "\tis_int: ";
  var_dump(is_int($a));

  echo "\tis_float: ";
  var_dump(is_float($a));

  echo "\tis_numeric: ";
  var_dump(is_numeric($a));

  echo "\tis_string: ";
  var_dump(is_string($a));

  echo "\tis_scalar: ";
  var_dump(is_scalar($a));

  echo "\tis_array: ";
  var_dump(is_array($a));

  echo "\tis_vec: ";
  var_dump(is_vec($a));

  echo "\tis_dict: ";
  var_dump(is_dict($a));

  echo "\tis_keyset: ";
  var_dump(is_keyset($a));

  echo "\tis_object: ";
  var_dump(is_object($a));

  echo "\tis_resource: ";
  var_dump(is_resource($a));

  echo "is Traversable: ";
  var_dump($a is Traversable);

  echo "is KeyedTraversable: ";
  var_dump($a is KeyedTraversable);

  echo "is Container: ";
  var_dump($a is Container);

  echo "is KeyedContainer: ";
  var_dump($a is KeyedContainer);

  echo "is XHPChild: ";
  var_dump($a is XHPChild);

  echo "is Vector: ";
  var_dump($a is Vector);

  echo "is Map: ";
  var_dump($a is Map);

  echo "is Foo: ";
  var_dump($a is Foo);

  foreach ($interfaces as $i) {
    echo "is (string) " . $i . ": ";
    var_dump(is_a($a, $i));
  }
}

function test_is_keyset($val) {
  echo "====================================================\n";
  echo "Testing for is_keyset: ";
  var_dump($val);
  if (is_keyset($val)) {
    echo "YES\n";
  } else {
    echo "NO\n";
  }
}

<<__EntryPoint>> function main(): void {
  $interfaces = varray[
    "HH\\Traversable",
    "HH\\KeyedTraversable",
    "HH\\Container",
    "HH\\KeyedContainer",
    "XHPChild",
    "Vector",
    "Map",
    "Foo",
  ];

  test_is_a(keyset[], $interfaces);
  test_is_a(keyset[123, "456", 789, "abc"], $interfaces);

  test_is_keyset(null);
  test_is_keyset(false);
  test_is_keyset(7);
  test_is_keyset(1.23);
  test_is_keyset("abcd");
  test_is_keyset(new stdclass);
  test_is_keyset(varray[1, 2, 3]);
  test_is_keyset(Vector{'a', 'b', 'c'});
  test_is_keyset(Map{100 => 'a', 'b' => 200});
  test_is_keyset(Pair{123, 'abc'});

  $resource = imagecreate(1, 1);
  test_is_keyset($resource);
  imagedestroy($resource);
}
