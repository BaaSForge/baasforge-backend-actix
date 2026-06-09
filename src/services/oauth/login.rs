use crate::configs::Settings;
use crate::database::{self, DbPool};
use crate::models::user_tokens::{UserTokenInternal, UserTokenType};
use crate::models::users::{PlatformType, User, UserDTO};
use crate::schema::users::{email_in_platform, full_name, id_in_platform, platform};
use crate::schema::{user_tokens, users};
use crate::services::auth::{
    ACCESS_TOKEN_EXPIRY_MINUTES, REFRESH_TOKEN_EXPIRY_MINUTES, create_access_cookie,
    create_access_token, create_refresh_cookie, create_refresh_token,
};
use actix_web::cookie::Cookie;
use chrono::{Duration, Utc};
use diesel::prelude::*;
use uuid::Uuid;

pub struct OAuthUserProfile {
    pub platform: PlatformType,
    pub id_in_platform: String,
    pub full_name: Option<String>,
    pub email_in_platform: Option<String>,
}

pub async fn complete_oauth_login(
    pool: &DbPool,
    settings: &Settings,
    profile: OAuthUserProfile,
) -> Result<(Cookie<'static>, Cookie<'static>), ()> {
    let access_token = create_access_token(settings).map_err(|_| ())?;
    let refresh_token = create_refresh_token(settings).map_err(|_| ())?;

    let now = Utc::now();
    let access_exp = now + Duration::minutes(ACCESS_TOKEN_EXPIRY_MINUTES as i64);
    let refresh_exp = now + Duration::minutes(REFRESH_TOKEN_EXPIRY_MINUTES as i64);

    let access_token_for_cookie = access_token.clone();
    let refresh_token_for_cookie = refresh_token.clone();

    let (access, refresh) = database::block_on_db(pool, move |conn| {
        let existing_user = users::table
            .filter(platform.eq(profile.platform))
            .filter(id_in_platform.eq(&profile.id_in_platform))
            .first::<User>(conn)
            .optional()?;

        let (user, is_just_signed_up) = if let Some(u) = existing_user {
            let updated = diesel::update(users::table.find(u.id))
                .set((
                    full_name.eq(profile.full_name.clone()),
                    email_in_platform.eq(profile.email_in_platform.clone()),
                ))
                .get_result::<User>(conn)?;
            (updated, false)
        } else {
            let new_user = diesel::insert_into(users::table)
                .values(&UserDTO {
                    platform: profile.platform,
                    id_in_platform: profile.id_in_platform,
                    full_name: profile.full_name,
                    email_in_platform: profile.email_in_platform,
                })
                .get_result::<User>(conn)?;
            (new_user, true)
        };

        let refresh_token_row = UserTokenInternal {
            user_id: user.id,
            token_type: UserTokenType::Refresh,
            token: refresh_token,
            expires_at: refresh_exp,
            revoked: Some(false),
            is_signup_token: is_just_signed_up,
            refresh_token_id: None,
        };
        let refresh_token_id: Uuid = diesel::insert_into(user_tokens::table)
            .values(&refresh_token_row)
            .returning(user_tokens::id)
            .get_result::<Uuid>(conn)?;

        let access_token_row = UserTokenInternal {
            user_id: user.id,
            token_type: UserTokenType::Access,
            token: access_token,
            expires_at: access_exp,
            revoked: Some(false),
            is_signup_token: is_just_signed_up,
            refresh_token_id: Some(refresh_token_id),
        };
        diesel::insert_into(user_tokens::table)
            .values(&access_token_row)
            .execute(conn)?;

        Ok::<_, diesel::result::Error>((access_token_for_cookie, refresh_token_for_cookie))
    })
    .await
    .map_err(|_| ())?;

    Ok((
        create_access_cookie(access),
        create_refresh_cookie(refresh),
    ))
}
