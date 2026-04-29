#[test]
fn dump_skip_keylet() {
    let k = rxrpl_protocol::keylet::skip();
    eprintln!("skip keylet = {}", k);
}
