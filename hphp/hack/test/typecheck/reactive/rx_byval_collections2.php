<?hh // strict

function f(varray<Map<int, int>> $a)[]: void {
  $a[0][1] = 5;
}
