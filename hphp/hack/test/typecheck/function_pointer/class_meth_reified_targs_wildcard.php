<?hh

final class Foo {
  public static function bar<reify T, T2>(T $_): void {}
}

function test(): void {
  $x = Foo::bar<int, _>;
}
