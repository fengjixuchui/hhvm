<?hh
<<__EntryPoint>>
function main_entry(): void {

  $db = new SQLite3(':memory:');
  $timenow = time();

  echo "Creating Table\n";
  var_dump($db->exec('CREATE TABLE test (time INTEGER, id STRING)'));

  echo "INSERT into table\n";
  var_dump($db->exec("INSERT INTO test (time, id) VALUES (" . $timenow . ", 'a')"));
  var_dump($db->exec("INSERT INTO test (time, id) VALUES (" . $timenow . ", 'b')"));

  echo "SELECTING results\n";
  $results = $db->query("SELECT * FROM test ORDER BY id ASC");
  while ($result = $results->fetcharray(SQLITE3_NUM))
  {
  	var_dump($result);
  }
  echo "Done\n";
}
