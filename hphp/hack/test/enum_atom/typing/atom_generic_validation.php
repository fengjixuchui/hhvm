<?hh
<<file: __EnableUnstableFeatures('enum_atom')>>

interface I {}
class C implements I {
  public function __construct(public int $x)[] {}
}

enum class E: I {
   C A = new C(42);
}

// Error, needs to be refied
function fgen<TEnum as E>(<<__Atom>> HH\MemberOf<TEnum, C> $x): int {
  return $x->x;
}

function fgen2<reify TEnum as E>(<<__Atom>> HH\MemberOf<TEnum, C> $x): int {
  return $x->x;
}

abstract class Controller {
  abstract const type TEnum as E;
  public static function get(<<__Atom>> HH\MemberOf<this::TEnum, C> $x): int {
    return $x->x;
  }
}

class XXX extends Controller {
  const type TEnum = E;
}

class YYY {
  public static function get(<<__Atom>> HH\MemberOf<XXX::TEnum, C> $x): int {
    return $x->x;
  }
}

class ZZZ {
  // error, invalid call to abstract class constant
  public static function get(
    <<__Atom>> HH\MemberOf<Controller::TEnum, C> $x,
  ): int {
    return $x->x;
  }
}
