use diesel::deserialize::{self, FromSql, FromSqlRow};
use diesel::expression::AsExpression;
use diesel::pg::{Pg, PgValue};
use diesel::Queryable;
use diesel::serialize::{self, IsNull, Output, ToSql};
use std::io::Write;
use diesel::Insertable;

#[derive(Debug, AsExpression, FromSqlRow)]
#[diesel(sql_type = crate::schema::sql_types::PlatformType)]
pub enum PlatformType {
  Google,
  Github,
}

impl ToSql<crate::schema::sql_types::PlatformType, Pg> for PlatformType {
  fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
    match *self {
      PlatformType::Google => out.write_all(b"google")?,
      PlatformType::Github => out.write_all(b"github")?,
    }
    Ok(IsNull::No)
  }
}

impl FromSql<crate::schema::sql_types::PlatformType, Pg> for PlatformType {
  fn from_sql(bytes: PgValue) -> deserialize::Result<Self> {
    match bytes.as_bytes() {
      b"google" => Ok(PlatformType::Google),
      b"github" => Ok(PlatformType::Github),
      _ => Err("Unrecognized enum variant".into()),
    }
  }
}

#[derive(Debug, Queryable, Insertable)]
#[diesel(table_name = crate::schema::users)]
pub struct User {
  pub platform: PlatformType,
  pub id_in_platform: String,
  pub full_name: Option<String>,
}