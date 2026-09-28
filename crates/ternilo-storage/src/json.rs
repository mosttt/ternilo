use serde::{Serialize, de::DeserializeOwned};
use sqlx::{
    Any, Decode, Encode, Type,
    any::{AnyTypeInfo, AnyValueRef},
    encode::IsNull,
    error::BoxDynError,
};

/// JSON is stored as text on both backends so the same typed queries are usable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Json<T>(pub T);

impl<T> Type<Any> for Json<T> {
    fn type_info() -> AnyTypeInfo {
        <String as Type<Any>>::type_info()
    }
}

impl<T: Serialize> Encode<'_, Any> for Json<T> {
    fn encode_by_ref(
        &self,
        buffer: &mut <Any as sqlx::Database>::ArgumentBuffer,
    ) -> Result<IsNull, BoxDynError> {
        <String as Encode<Any>>::encode(serde_json::to_string(&self.0)?, buffer)
    }
}

impl<'r, T: DeserializeOwned> Decode<'r, Any> for Json<T> {
    fn decode(value: AnyValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(Self(serde_json::from_str(<&str as Decode<Any>>::decode(
            value,
        )?)?))
    }
}
