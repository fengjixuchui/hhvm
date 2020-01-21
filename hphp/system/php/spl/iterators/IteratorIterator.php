<?hh // partial

// This doc comment block generated by idl/sysdoc.php
/**
 * ( excerpt from http://php.net/manual/en/class.iteratoriterator.php )
 *
 * This iterator wrapper allows the conversion of anything that is
 * Traversable into an Iterator. It is important to understand that most
 * classes that do not implement Iterators have reasons as most likely they
 * do not allow the full Iterator feature set. If so, techniques should be
 * provided to prevent misuse, otherwise expect exceptions or fatal errors.
 *
 */
class IteratorIterator implements OuterIterator {
  private $iterator;
  private $current;
  private $key;
  private $position;

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from http://php.net/manual/en/iteratoriterator.construct.php )
   *
   * Creates an iterator from anything that is traversable.
   *
   * @iterator   mixed   The traversable iterator.
   *
   * @return     mixed   No value is returned.
   */
  public function __construct($iterator) {
    while ($iterator is IteratorAggregate) {
      $iterator = $iterator->getIterator();
    }
    if ($iterator is \HH\Iterator) {
      $this->iterator = $iterator;
    } else if ($iterator is \SimpleXMLElement) {
      $this->iterator = $iterator->getIterator();
    } else {
      throw new Exception(
        "Need to pass a Traversable that is convertable to an iterator");
    }
  }

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from
   * http://php.net/manual/en/iteratoriterator.getinneriterator.php )
   *
   * Get the inner iterator.
   *
   * @return     mixed   The inner iterator as passed to
   *                     IteratorIterator::__construct().
   */
  public function getInnerIterator() {
    return $this->iterator;
  }

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from http://php.net/manual/en/iteratoriterator.valid.php )
   *
   * Checks if the iterator is valid.
   *
   * @return     mixed   Returns TRUE if the iterator is valid, otherwise
   *                     FALSE
   */
  public function valid() {
    return $this->iterator->valid();
  }

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from http://php.net/manual/en/iteratoriterator.key.php )
   *
   * Get the key of the current element.
   *
   * @return     mixed   The key of the current element.
   */
  public function key() {
    return $this->key;
  }

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from http://php.net/manual/en/iteratoriterator.current.php )
   *
   * Get the value of the current element.
   *
   * @return     mixed   The value of the current element.
   */
  public function current() {
    return $this->current;
  }

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from http://php.net/manual/en/iteratoriterator.next.php )
   *
   * Forward to the next element.
   *
   * @return     mixed   No value is returned.
   */
  public function next() {
    $this->iterator->next();
    $this->position++;
    $this->_fetch(true);
    return;
  }

  // This doc comment block generated by idl/sysdoc.php
  /**
   * ( excerpt from http://php.net/manual/en/iteratoriterator.rewind.php )
   *
   * Rewinds to the first element.
   *
   * @return     mixed   No value is returned.
   */
  public function rewind() {
    $this->iterator->rewind();
    $this->position = 0;
    $this->_fetch(true);
    return;
  }

  public function __call($func, $params) {
    return call_user_func_array(array($this->iterator, $func), $params);
  }

  /**
   * This function appears in the php source in spl_iterators.c as
   * spl_dual_it_fetch. Apparently, all iterators that store other
   * iterators are forced to do this layer of caching. If you call
   * next(), these "dual" iterators will need to get the key and
   * current value out of the underlying iterator and store it.
   *
   * Basically, if you see a call to spl_dual_it_fetch in the
   * PHP source, it's very likely that you should call this.
   */
  protected function _fetch($check) {
    if (!$check || $this->iterator->valid()) {
      $this->current = $this->iterator->current();
      $key = $this->iterator->key();
      $this->key = is_null($key) ? $this->position : $key;
      return true;
    } else {
      $this->current = null;
      $this->key = null;
      return false;
    }
  }

  protected function _getPosition() {
    return $this->position;
  }

  protected function _setPosition($position) {
    $this->position = $position;
  }

}
