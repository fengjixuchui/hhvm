<?hh
<<__EntryPoint>>
function main() {
  $x = new DomDocument();
  try { $x->createComment(); } catch (Exception $e) { echo "\n".'Warning: '.$e->getMessage().' in '.__FILE__.' on line '.__LINE__."\n"; }
  echo "===DONE===\n";
}
