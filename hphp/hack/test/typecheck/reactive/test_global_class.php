<?hh // strict
class GlobalClassName {
  public static int $x = 0;

}

<<__Rx>>
function foo()[rx]: void {
  $y = GlobalClassName::$x + 1;
}
