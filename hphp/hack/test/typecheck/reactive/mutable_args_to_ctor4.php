<?hh
<<file: __EnableUnstableFeatures('coeffects_provisional')>>

<<__ConsistentConstruct>>
class A {
  <<__Rx>>
  public function __construct(<<__Mutable>>this $a) {
  }

  <<__Rx, __Mutable>>
  public function make(): this {
    // OK, passing mutable as mutable
    return new static($this);
  }
}
