<?hh

interface I1 {
  abstract const type T = int;
}
interface I2 {
  abstract const type T = string;
}

// error, must redeclare to choose default or as concrete
abstract class A implements I1, I2 {}

<<__EntryPoint>>
function main(): void {
  echo "TODO(T89444033) should be error\n";
}
