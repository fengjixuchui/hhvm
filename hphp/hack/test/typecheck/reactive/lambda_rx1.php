<?hh

function f(): void {
  // OK: lambda is rx, can call rx
  $a = () ==> {
    f();
  };
}
