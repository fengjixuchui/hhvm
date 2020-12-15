<?hh // strict
<<file: __EnableUnstableFeatures('coeffects_provisional')>>
class A {
  protected darray<string, int> $events = darray[];

  <<__RxShallow>>
  public function eventMutable(?string $event): void {
    if ($event !== null) {
      // ERROR
      $this->events[$event] = 1;
    }
  }
}
