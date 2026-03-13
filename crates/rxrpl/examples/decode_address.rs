use rxrpl::codec::address::{
    classic::{decode_account_id, encode_account_id, is_valid_classic_address},
    xaddress::{decode_x_address, encode_x_address, is_valid_x_address},
};

fn main() {
    let classic = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    println!("Classic address: {classic}");
    println!("  Valid classic? {}", is_valid_classic_address(classic));
    println!("  Valid X-addr?  {}", is_valid_x_address(classic));

    // Decode classic to account ID
    let account_id = decode_account_id(classic).expect("invalid address");
    println!("  Account ID:    {account_id}");

    // Encode as X-address (mainnet, no tag)
    let x_addr = encode_x_address(&account_id, None, false);
    println!("\nX-address (no tag): {x_addr}");

    // Encode with tag
    let x_addr_tag = encode_x_address(&account_id, Some(12345), false);
    println!("X-address (tag=12345): {x_addr_tag}");

    // Decode X-address back
    let (decoded_id, tag, is_test) = decode_x_address(&x_addr_tag).expect("invalid X-address");
    println!("\nDecoded X-address:");
    println!("  Account ID: {decoded_id}");
    println!("  Tag:        {tag:?}");
    println!("  Test net:   {is_test}");

    // Roundtrip back to classic
    let classic_again = encode_account_id(&decoded_id);
    println!("  Classic:    {classic_again}");
    assert_eq!(classic, classic_again);
}
