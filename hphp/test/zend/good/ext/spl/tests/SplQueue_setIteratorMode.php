<?hh <<__EntryPoint>> function main(): void {
$queue = new SplQueue();
try {
  $queue->setIteratorMode(SplDoublyLinkedList::IT_MODE_LIFO);
} catch (Exception $e) {
  echo $e->getMessage();
}
}
