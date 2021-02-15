open Integration_test_base_types
module Test = Integration_test_base

let file_a = "A.php"

let file_b = "B.php"

let content_a_0 =
  {|<?hh // strict
interface Rx {
  public function f(): int;
}
|}

let content_a_1 =
  {|<?hh // strict
interface Rx {
  <<__Pure>>
  public function f(): int;
}
|}

let content_b =
  {|<?hh // strict
class A {
  <<__Pure, __OnlyRxIfImpl(Rx::class)>>
  public function f(): int {
    return 5;
  }
}

class B extends A {
  public function f(): int {
    return 6;
  }
}
|}

let errors =
  {|
File "/B.php", line 10, characters 19-19:
The method `f` is not compatible with the overridden method (Typing[4341])
  File "/B.php", line 4, characters 19-19:
  This function is conditionally pure (condition type: \Rx).
  File "/B.php", line 10, characters 19-19:
  This function is normal.
|}

let test () =
  let env = Test.setup_server () in
  let env = Test.setup_disk env [(file_a, content_a_0); (file_b, content_b)] in
  Test.assert_env_errors env errors;

  let (env, loop_output) =
    Test.(
      run_loop_once
        env
        { default_loop_input with disk_changes = [(file_a, content_a_1)] })
  in
  if not loop_output.did_read_disk_changes then
    Test.fail "Expected the server to process disk updates";

  Test.assert_no_errors env
