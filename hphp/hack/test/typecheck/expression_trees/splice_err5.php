<?hh

<<file:__EnableUnstableFeatures('expression_trees')>>

function foo(): void {
  $x = Code`'Hello'`;
  $y = Code`4`;

  $z = Code`4 + ${$x}`;
}
