<?hh
class C {}

<<__Rx>>
function f(): C {
    $a = \HH\Rx\mutable(new C());
    try {
        return $a;
    }
    finally {
        h($a);
    }
}

<<__Rx>>
function h(<<__MaybeMutable>> C $c): void {
}

<<__Rx, __MutableReturn>>
function g(<<__OwnedMutable>> C $c): C {
    return $c;
}
