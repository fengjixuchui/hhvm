<?hh
class B {
  private $y=2;
}
class C extends B {
  public $w=3;
  function dump_vars() {
    var_dump(get_class_vars('C'));
  }
}
$o = new C;
$o->dump_vars();

class D {
  public $x = NonExistentClass::FOO;
}
class E extends D {
  public $x = 123;
}
function main() {
  $d = new E();
  echo "Done\n";
}
main();

class G {
  public static $x = NonExistentClass::FOO;
}

class H extends G {
  public static $x = F::FOO;
}

function main2() {
  HH\autoload_set_paths(
    dict[
      'class' => dict[
        'f' => 'propeties4.inc',
      ],
    ],
    __DIR__.'/',
  );

  $d = new H();
  echo "Done\n";
}
main2();
