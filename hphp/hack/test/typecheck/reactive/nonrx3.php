<?hh
<<file: __EnableUnstableFeatures('coeffects_provisional')>>

// OK
<<__NonRx("never rx")>>
function f(): void {
}

// OK
<<__NonRx('never rx')>>
function g(): void {
}
