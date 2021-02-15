<?hh

type ExprTreeInferredType<T> = ?(function(): T);

class Code {
  const type TAst = mixed;

  public static function makeTree<TVisitor as Code, TInfer>(
    ?ExprPos $pos,
    string $filepath,
    dict<string, mixed> $spliced_values,
    (function(TVisitor): Code::TAst) $ast,
    ExprTreeInferredType<TInfer> $null,
  ): ExprTree<TVisitor, Code::TAst, TInfer> {
    throw new Exception();
  }

  // Lifting literals.
  public static function intLiteral(
    int $_,
  ): ExprTree<Code, Code::TAst, ExampleInt> {
    throw new Exception();
  }
  public static function floatLiteral(
    float $_,
  ): ExprTree<Code, Code::TAst, ExampleFloat> {
    throw new Exception();
  }
  public static function boolLiteral(bool $_):
    ExprTree<Code, Code::TAst, ExampleBool>
  {
    throw new Exception();
  }
  public static function stringLiteral(string $_):
    ExprTree<Code, Code::TAst, ExampleString>
  {
    throw new Exception();
  }
  public static function nullLiteral(): ExprTree<Code, Code::TAst, null> {
    throw new Exception();
  }
  public static function voidLiteral(): ExprTree<Code, Code::TAst, ExampleVoid> {
    throw new Exception();
  }

  // Symbols
  public static function symbol<T>(
    string $_,
    (function(ExampleContext): Awaitable<ExprTree<Code, Code::TAst, T>>) $_,
  ): ExprTree<Code, Code::TAst, T> {
    throw new Exception();
  }

  // Expressions
  public function localVar(?ExprPos $_, string $_): Code::TAst {
    throw new Exception();
  }
  public function lambdaLiteral(
    ?ExprPos $_,
    vec<string> $_args,
    vec<Code::TAst> $_body,
  ): Code::TAst {
    throw new Exception();
  }

  // Operators
  public function methCall(
    ?ExprPos $_,
    Code::TAst $_,
    string $_,
    vec<Code::TAst> $_,
  ): Code::TAst {
    throw new Exception();
  }

  // Old style operators
  public function call<T>(
    ?ExprPos $_,
    Code::TAst $_callee,
    vec<Code::TAst> $_args,
  ): Code::TAst {
    throw new Exception();
  }

  public function assign(
    ?ExprPos $_,
    Code::TAst $_,
    Code::TAst $_,
  ): Code::TAst {
    throw new Exception();
  }

  public function ternary(
    ?ExprPos $_,
    Code::TAst $_condition,
    ?Code::TAst $_truthy,
    Code::TAst $_falsy,
  ): Code::TAst {
    throw new Exception();
  }

  // Statements.
  public function ifStatement(
    ?ExprPos $_,
    Code::TAst $_cond,
    vec<Code::TAst> $_then_body,
    vec<Code::TAst> $_else_body,
  ): Code::TAst {
    throw new Exception();
  }
  public function whileStatement(
    ?ExprPos $_,
    Code::TAst $_cond,
    vec<Code::TAst> $_body,
  ): Code::TAst {
    throw new Exception();
  }
  public function returnStatement(
    ?ExprPos $_,
    ?Code::TAst $_,
  ): Code::TAst {
    throw new Exception();
  }
  public function forStatement(
    ?ExprPos $_,
    vec<Code::TAst> $_,
    Code::TAst $_,
    vec<Code::TAst> $_,
    vec<Code::TAst> $_,
  ): Code::TAst {
    throw new Exception();
  }
  public function breakStatement(?ExprPos $_): Code::TAst {
    throw new Exception();
  }
  public function continueStatement(?ExprPos $_,): Code::TAst {
    throw new Exception();
  }

  // Splice
  public function splice<T>(
    ?ExprPos $_,
    string $_key,
    Spliceable<Code, Code::TAst, T> $_,
  ): Code::TAst {
    throw new Exception();
  }
}

interface Spliceable<TVisitor, TResult, +TInfer> {
  public function visit(TVisitor $v): TResult;
}

final class ExprTree<TVisitor, TResult, +TInfer>
  implements Spliceable<TVisitor, TResult, TInfer> {
  public function __construct(
    private ?ExprPos $pos,
    private string $filepath,
    private dict<string, mixed> $spliced_values,
    private (function(TVisitor): TResult) $ast,
    private (function(): TInfer) $err,
  ) {}

  public function visit(TVisitor $v): TResult {
    return ($this->ast)($v);
  }
}

final class ExprPos {
  public function __construct(
    private int $begin_line,
    private int $begin_col,
    private int $end_line,
    private int $end_col,
  ) {}
}

abstract class ExampleMixed {
  public abstract function __tripleEquals(ExampleMixed $_): ExampleBool;
  public abstract function __notTripleEquals(ExampleMixed $_): ExampleBool;
}
abstract class ExampleInt extends ExampleMixed {
  public abstract function __plus(ExampleInt $_): ExampleInt;
  public abstract function __minus(ExampleInt $_): ExampleInt;
  public abstract function __star(ExampleInt $_): ExampleInt;
  public abstract function __slash(ExampleInt $_): ExampleInt;

  public abstract function __lessThan(ExampleInt $_): ExampleBool;
  public abstract function __lessThanEqual(ExampleInt $_): ExampleBool;
  public abstract function __greaterThan(ExampleInt $_): ExampleBool;
  public abstract function __greaterThanEqual(ExampleInt $_): ExampleBool;
}

abstract class ExampleBool extends ExampleMixed {
  public abstract function __ampamp(ExampleBool $_): ExampleBool;
  public abstract function __barbar(ExampleBool $_): ExampleBool;
  public abstract function __bool(): bool;
  public abstract function __exclamationMark(): ExampleBool;
}

abstract class ExampleString extends ExampleMixed {}
abstract class ExampleFloat extends ExampleMixed {}

final class ExampleContext {}

abstract class ExampleVoid {}
