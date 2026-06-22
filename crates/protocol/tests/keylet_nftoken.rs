//! Golden keylets for NFToken offers and their buy/sell directories, taken from
//! mainnet transaction F68078BF…B40157 (ledger 105093479).

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;

fn nftoken_id() -> Hash256 {
    let bytes: [u8; 32] =
        hex::decode("00181F40B212084949C4428A5C77B4D9AFE7843D18EEC349BD879DDC05F5B242")
            .unwrap()
            .try_into()
            .unwrap();
    Hash256::new(bytes)
}

#[test]
fn nftoken_offer_index_matches_mainnet() {
    let owner = decode_account_id("rMTaAGVd77vAbzDv8qhoMgnZR1T5R3w6Wq").unwrap();
    let key = keylet::nftoken_offer(&owner, 103662424);
    assert_eq!(
        key.to_string().to_uppercase(),
        "414588CB83779B5BD7645D36C79AA166CA95F5C2A06BBC1B9F0B985FB4E5F9FC"
    );
}

#[test]
fn nft_buys_directory_matches_mainnet() {
    assert_eq!(
        keylet::nft_buys(&nftoken_id()).to_string().to_uppercase(),
        "37BB8D4C73153F6F56A04EAC2CB09337F339064AA5D430E8075D62F254D87126"
    );
}

#[test]
fn nft_buys_and_sells_differ() {
    let id = nftoken_id();
    assert_ne!(keylet::nft_buys(&id), keylet::nft_sells(&id));
}
