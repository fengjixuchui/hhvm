<?hh // strict
<<file: __EnableUnstableFeatures('coeffects_provisional')>>

<<__Rx>>
function f(): void {
  nonrx(); // should error
}

<<__Rx>>
function rx(): void {
}

function nonrx(): void {
}
