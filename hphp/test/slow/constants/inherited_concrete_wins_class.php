<?hh

abstract class A {
  const type T = float;
}
interface I {
  abstract const type T = int;
}
class C extends A implements I {}

<<__EntryPoint>>
function main(): void {
  // expecting TypeStructureKind::OF_FLOAT = 3
  var_dump(type_structure(C::class, "T"));
}
