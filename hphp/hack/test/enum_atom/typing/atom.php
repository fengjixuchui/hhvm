<?hh
<<file:__EnableUnstableFeatures('enum_atom')>>

enum E : int as int {
  A = 42;
  B = 1664;
}

// Not allowed on enums at the moment
function f(<<__Atom>> E $x): int {
  return $x;
}

interface I {}
class Box implements I {
  public function __construct(public int $x) {}
}

enum class EE : I {
   Box A = new Box(42);
}

function ff(<<__Atom>> HH\MemberOf<EE, Box> $z) : int {
  return $z->x;
}

<<__EntryPoint>>
function main(): void {
  ff(#A);
  ff(#C); // unknown constant
}

class C {}

function wrong_upper_bound<reify T as C>(
    <<__Atom>> HH\MemberOf<T, Box> $w
  ): mixed {
  return $w->x;
}
