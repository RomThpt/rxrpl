/// Macro to define a transaction type with flattened common fields.
///
/// Usage:
/// ```ignore
/// define_transaction! {
///     /// A Payment transaction.
///     Payment => TransactionType::Payment,
///     {
///         "Destination" => destination: String,
///         "Amount" => amount: serde_json::Value,
///         optional "DestinationTag" => destination_tag: Option<u32>,
///     }
/// }
/// ```
macro_rules! define_transaction {
    (
        $(#[$meta:meta])*
        $name:ident => $tt:expr,
        {
            $(
                $(#[$fmeta:meta])*
                $json_key:literal => $field:ident : $ftype:ty
            ),*
            $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        pub struct $name {
            /// Common fields shared by all transaction types.
            #[serde(flatten)]
            pub common: $crate::tx::common::CommonFields,

            $(
                $(#[$fmeta])*
                #[serde(rename = $json_key)]
                pub $field: $ftype,
            )*
        }

        impl $crate::tx::common::Transaction for $name {
            fn transaction_type() -> $crate::types::TransactionType {
                $tt
            }

            fn common(&self) -> &$crate::tx::common::CommonFields {
                &self.common
            }

            fn common_mut(&mut self) -> &mut $crate::tx::common::CommonFields {
                &mut self.common
            }
        }
    };
}

pub(crate) use define_transaction;
