<?hh

include 'async-implicit.inc';

async function printImplicit() {
  echo "Implicit: " . (string) IntContext::getContext() . "\n";
}

async function aux() {
  $x = IntContext::getContext();
  var_dump($x);
  await IntContext::genStart($x+1, fun('printImplicit'));
  var_dump(IntContext::getContext());
}

<<__EntryPoint>>
async function main() {
  await IntContext::genStart(0, fun('aux'));
}
