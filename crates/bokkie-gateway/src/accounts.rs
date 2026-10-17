//! Accounts, sessions and invitations in the database.
//!
//! Every function that reads or changes one user's data takes that user's
//! [`UserId`] and puts it in the query's `WHERE` clause. Admin-only functions
//! say so in their names.

use bokkie_protocol::api::{AdminUser, InvitationSummary, Limits, Role, SessionSummary, User};
use bokkie_protocol::{Timestamp, UserId};
use sqlx::{Row, SqlitePool};

use crate::clock::DAY_MS;
use crate::error::{Failure, Result};
use crate::secrets;

/// A session lasts this long after its last use.
pub const SESSION_IDLE_MS: i64 = 400 * DAY_MS;
/// An invitation's default and longest lifetime.
pub const INVITATION_DEFAULT_MS: i64 = 3 * DAY_MS;
pub const INVITATION_MAX_MS: i64 = 30 * DAY_MS;

/// A user row, including what only the gateway may see.
#[derive(Debug, Clone)]
pub struct Account {
    pub user: User,
    pub limits: Limits,
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Member => "member",
    }
}

fn role_from(name: &str) -> Role {
    if name == "admin" {
        Role::Admin
    } else {
        Role::Member
    }
}

const ACCOUNT_COLUMNS: &str =
    "id, username, display_name, role, disabled, created_at, daily_turns, weekly_tokens";

fn account_from(row: &sqlx::sqlite::SqliteRow) -> Account {
    Account {
        user: User {
            id: UserId(row.get("id")),
            username: row.get("username"),
            display_name: row.get("display_name"),
            role: role_from(row.get::<&str, _>("role")),
            disabled: row.get::<i64, _>("disabled") != 0,
            created_at: Timestamp(row.get("created_at")),
        },
        limits: Limits {
            daily_turns: row.get::<Option<i64>, _>("daily_turns").map(|v| v as u32),
            weekly_tokens: row.get::<Option<i64>, _>("weekly_tokens").map(|v| v as u64),
        },
    }
}

// ---------------------------------------------------------------------------
// Validation

/// Normalises and checks a username: 2 to 32 characters of `a-z`, `0-9`,
/// `.`, `_` and `-`, starting with a letter or digit.
pub fn normalise_username(username: &str) -> Result<String> {
    let username = username.trim().to_ascii_lowercase();
    let valid = (2..=32).contains(&username.len())
        && username
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        && username
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric());
    if valid {
        Ok(username)
    } else {
        Err(Failure::invalid(
            "Usernames have 2 to 32 letters, digits, dots, dashes or underscores",
        ))
    }
}

/// Trims and checks a display name: 1 to 64 characters, no control
/// characters.
pub fn normalise_display_name(name: &str) -> Result<String> {
    let name = name.trim().to_owned();
    if (1..=64).contains(&name.chars().count()) && !name.chars().any(char::is_control) {
        Ok(name)
    } else {
        Err(Failure::invalid("Display names have 1 to 64 characters"))
    }
}

fn check_password(password: &str) -> Result<()> {
    secrets::check_new_password(password).map_err(Failure::invalid)
}

async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || secrets::hash_password(&password))
        .await
        .map_err(Failure::internal)
}

// ---------------------------------------------------------------------------
// Users

/// Creates the first admin. Fails if an admin already exists.
pub async fn bootstrap_admin(
    db: &SqlitePool,
    now: Timestamp,
    username: &str,
    display_name: &str,
    password: &str,
) -> Result<User> {
    let username = normalise_username(username)?;
    let display_name = normalise_display_name(display_name)?;
    check_password(password)?;
    let hash = hash_password(password).await?;
    let mut tx = db.begin().await?;
    let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE role = 'admin'")
        .fetch_one(&mut *tx)
        .await?;
    if admins > 0 {
        return Err(Failure::conflict("An admin already exists"));
    }
    let id = insert_user(&mut tx, now, &username, &display_name, Role::Admin, &hash).await?;
    tx.commit().await?;
    Ok(account(db, id).await?.ok_or_else(Failure::not_found)?.user)
}

async fn insert_user(
    tx: &mut sqlx::SqliteConnection,
    now: Timestamp,
    username: &str,
    display_name: &str,
    role: Role,
    password_hash: &str,
) -> Result<UserId> {
    let result = sqlx::query(
        "INSERT INTO users (username, display_name, role, password_hash, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(username)
    .bind(display_name)
    .bind(role_name(role))
    .bind(password_hash)
    .bind(now.0)
    .bind(now.0)
    .execute(&mut *tx)
    .await;
    match result {
        Ok(done) => Ok(UserId(done.last_insert_rowid())),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(Failure::conflict("That username is taken"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn account(db: &SqlitePool, user: UserId) -> Result<Option<Account>> {
    let row = sqlx::query(&format!("SELECT {ACCOUNT_COLUMNS} FROM users WHERE id = ?"))
        .bind(user.0)
        .fetch_optional(db)
        .await?;
    Ok(row.as_ref().map(account_from))
}

/// Every account, for an admin.
pub async fn admin_list_users(db: &SqlitePool) -> Result<Vec<AdminUser>> {
    let rows = sqlx::query(&format!("SELECT {ACCOUNT_COLUMNS} FROM users ORDER BY id"))
        .fetch_all(db)
        .await?;
    Ok(rows
        .iter()
        .map(account_from)
        .map(|a| AdminUser {
            user: a.user,
            limits: a.limits,
        })
        .collect())
}

/// Changes another account's display name, disabled flag or limits, for an
/// admin. Disabling revokes every session of that account.
pub async fn admin_update_user(
    db: &SqlitePool,
    now: Timestamp,
    admin: UserId,
    target: UserId,
    display_name: Option<&str>,
    disabled: Option<bool>,
    limits: Option<Limits>,
) -> Result<AdminUser> {
    if target == admin && disabled == Some(true) {
        return Err(Failure::invalid("You cannot disable your own account"));
    }
    let display_name = display_name.map(normalise_display_name).transpose()?;
    let mut tx = db.begin().await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(target.0)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(Failure::not_found());
    }
    if let Some(name) = display_name {
        sqlx::query("UPDATE users SET display_name = ?, updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(now.0)
            .bind(target.0)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(disabled) = disabled {
        sqlx::query("UPDATE users SET disabled = ?, updated_at = ? WHERE id = ?")
            .bind(disabled as i64)
            .bind(now.0)
            .bind(target.0)
            .execute(&mut *tx)
            .await?;
        if disabled {
            revoke_all_sessions(&mut tx, now, target, None).await?;
        }
    }
    if let Some(limits) = limits {
        sqlx::query(
            "UPDATE users SET daily_turns = ?, weekly_tokens = ?, updated_at = ? WHERE id = ?",
        )
        .bind(limits.daily_turns.map(i64::from))
        .bind(limits.weekly_tokens.map(|v| v.min(i64::MAX as u64) as i64))
        .bind(now.0)
        .bind(target.0)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let account = account(db, target).await?.ok_or_else(Failure::not_found)?;
    Ok(AdminUser {
        user: account.user,
        limits: account.limits,
    })
}

/// Sets a new password for an account, for an admin. Revokes every session
/// of that account.
pub async fn admin_reset_password(
    db: &SqlitePool,
    now: Timestamp,
    target: UserId,
    new_password: &str,
) -> Result<()> {
    check_password(new_password)?;
    let hash = hash_password(new_password).await?;
    let mut tx = db.begin().await?;
    let updated = sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(hash)
        .bind(now.0)
        .bind(target.0)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(Failure::not_found());
    }
    revoke_all_sessions(&mut tx, now, target, None).await?;
    tx.commit().await?;
    Ok(())
}

/// Changes the signed-in user's own password after checking the current
/// one. Revokes every other session of that user.
pub async fn change_own_password(
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    current_session: i64,
    current_password: &str,
    new_password: &str,
) -> Result<()> {
    check_password(new_password)?;
    let stored: Option<String> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(user.0)
        .fetch_optional(db)
        .await?;
    let current = current_password.to_owned();
    let ok =
        tokio::task::spawn_blocking(move || secrets::verify_password(&current, stored.as_deref()))
            .await
            .map_err(Failure::internal)?;
    if !ok {
        return Err(Failure::forbidden("The current password is wrong"));
    }
    let hash = hash_password(new_password).await?;
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(hash)
        .bind(now.0)
        .bind(user.0)
        .execute(&mut *tx)
        .await?;
    revoke_all_sessions(&mut tx, now, user, Some(current_session)).await?;
    tx.commit().await?;
    Ok(())
}

/// Checks a username and password. Returns the account only when the
/// password matches and the account is enabled.
pub async fn check_credentials(
    db: &SqlitePool,
    username: &str,
    password: &str,
) -> Result<Option<Account>> {
    let username = username.trim().to_ascii_lowercase();
    let row = sqlx::query(&format!(
        "SELECT {ACCOUNT_COLUMNS}, password_hash FROM users WHERE username = ?"
    ))
    .bind(&username)
    .fetch_optional(db)
    .await?;
    let stored: Option<String> = row.as_ref().map(|r| r.get("password_hash"));
    let password = password.to_owned();
    let ok =
        tokio::task::spawn_blocking(move || secrets::verify_password(&password, stored.as_deref()))
            .await
            .map_err(Failure::internal)?;
    Ok(row
        .as_ref()
        .filter(|_| ok)
        .map(account_from)
        .filter(|a| !a.user.disabled))
}

// ---------------------------------------------------------------------------
// Sessions

/// A signed-in session, found from its cookie.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: i64,
    pub account: Account,
}

/// Creates a session and returns its cookie token.
pub async fn create_session(
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    user_agent: Option<&str>,
) -> Result<String> {
    let token = secrets::random_token();
    let user_agent = user_agent.map(|ua| ua.chars().take(120).collect::<String>());
    sqlx::query(
        "INSERT INTO sessions (user_id, token_hash, user_agent, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user.0)
    .bind(secrets::token_hash(&token))
    .bind(user_agent)
    .bind(now.0)
    .bind(now.0)
    .bind(now.0 + SESSION_IDLE_MS)
    .execute(db)
    .await?;
    Ok(token)
}

/// Finds the live session for a cookie token: not revoked, not expired, and
/// its account not disabled. Extends the session when it was last seen more
/// than an hour ago.
pub async fn find_session(db: &SqlitePool, now: Timestamp, token: &str) -> Result<Option<Session>> {
    let columns = ACCOUNT_COLUMNS
        .split(", ")
        .map(|c| format!("u.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(&format!(
        "SELECT s.id AS session_id, s.last_seen_at, {columns}
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ? AND s.revoked_at IS NULL AND s.expires_at > ? AND u.disabled = 0"
    ))
    .bind(secrets::token_hash(token))
    .bind(now.0)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: i64 = row.get("session_id");
    let last_seen: i64 = row.get("last_seen_at");
    if now.0 - last_seen > crate::clock::HOUR_MS {
        sqlx::query("UPDATE sessions SET last_seen_at = ?, expires_at = ? WHERE id = ?")
            .bind(now.0)
            .bind(now.0 + SESSION_IDLE_MS)
            .bind(id)
            .execute(db)
            .await?;
    }
    Ok(Some(Session {
        id,
        account: account_from(&row),
    }))
}

/// The user's live sessions.
pub async fn list_sessions(
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    current: i64,
) -> Result<Vec<SessionSummary>> {
    let rows = sqlx::query(
        "SELECT id, created_at, last_seen_at, user_agent FROM sessions
         WHERE user_id = ? AND revoked_at IS NULL AND expires_at > ? ORDER BY id",
    )
    .bind(user.0)
    .bind(now.0)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SessionSummary {
            id: r.get("id"),
            current: r.get::<i64, _>("id") == current,
            created_at: Timestamp(r.get("created_at")),
            last_seen_at: Timestamp(r.get("last_seen_at")),
            user_agent: r.get("user_agent"),
        })
        .collect())
}

/// Revokes one of the user's own sessions. Another user's session is
/// reported as not found.
pub async fn revoke_session(
    db: &SqlitePool,
    now: Timestamp,
    user: UserId,
    session: i64,
) -> Result<()> {
    let done = sqlx::query(
        "UPDATE sessions SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(now.0)
    .bind(session)
    .bind(user.0)
    .execute(db)
    .await?;
    if done.rows_affected() == 0 {
        return Err(Failure::not_found());
    }
    Ok(())
}

async fn revoke_all_sessions(
    tx: &mut sqlx::SqliteConnection,
    now: Timestamp,
    user: UserId,
    except: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "UPDATE sessions SET revoked_at = ?
         WHERE user_id = ? AND revoked_at IS NULL AND id IS NOT ?",
    )
    .bind(now.0)
    .bind(user.0)
    .bind(except)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Invitations

/// Creates an invitation, for an admin. Returns its id, code and expiry.
pub async fn admin_create_invitation(
    db: &SqlitePool,
    now: Timestamp,
    admin: UserId,
    role: Role,
    display_name: Option<&str>,
    expires_in_hours: Option<u32>,
) -> Result<(i64, String, Timestamp)> {
    let display_name = display_name.map(normalise_display_name).transpose()?;
    let lifetime = expires_in_hours
        .map(|h| i64::from(h) * crate::clock::HOUR_MS)
        .unwrap_or(INVITATION_DEFAULT_MS);
    if lifetime <= 0 || lifetime > INVITATION_MAX_MS {
        return Err(Failure::invalid("Invitations last from 1 hour to 30 days"));
    }
    let code = secrets::random_token();
    let expires_at = Timestamp(now.0 + lifetime);
    let done = sqlx::query(
        "INSERT INTO invitations (code_hash, role, display_name, created_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(secrets::token_hash(&code))
    .bind(role_name(role))
    .bind(display_name)
    .bind(admin.0)
    .bind(now.0)
    .bind(expires_at.0)
    .execute(db)
    .await?;
    Ok((done.last_insert_rowid(), code, expires_at))
}

/// Outstanding invitations, for an admin.
pub async fn admin_list_invitations(
    db: &SqlitePool,
    now: Timestamp,
) -> Result<Vec<InvitationSummary>> {
    let rows = sqlx::query(
        "SELECT id, role, display_name, created_at, expires_at FROM invitations
         WHERE used_at IS NULL AND withdrawn_at IS NULL AND expires_at > ? ORDER BY id",
    )
    .bind(now.0)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| InvitationSummary {
            id: r.get("id"),
            role: role_from(r.get::<&str, _>("role")),
            display_name: r.get("display_name"),
            created_at: Timestamp(r.get("created_at")),
            expires_at: Timestamp(r.get("expires_at")),
        })
        .collect())
}

/// Withdraws an outstanding invitation, for an admin.
pub async fn admin_withdraw_invitation(db: &SqlitePool, now: Timestamp, id: i64) -> Result<()> {
    let done = sqlx::query(
        "UPDATE invitations SET withdrawn_at = ?
         WHERE id = ? AND used_at IS NULL AND withdrawn_at IS NULL",
    )
    .bind(now.0)
    .bind(id)
    .execute(db)
    .await?;
    if done.rows_affected() == 0 {
        return Err(Failure::not_found());
    }
    Ok(())
}

/// Creates an account from an invitation code and consumes the code. Every
/// failure about the code itself reads the same, so codes cannot be probed.
pub async fn accept_invitation(
    db: &SqlitePool,
    now: Timestamp,
    code: &str,
    username: &str,
    display_name: &str,
    password: &str,
) -> Result<UserId> {
    let username = normalise_username(username)?;
    let display_name = normalise_display_name(display_name)?;
    check_password(password)?;
    let invalid = || Failure::forbidden("That invitation is not valid");
    let hash_of_code = secrets::token_hash(code.trim());
    // Check the code before spending time on the password hash.
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM invitations
         WHERE code_hash = ? AND used_at IS NULL AND withdrawn_at IS NULL AND expires_at > ?",
    )
    .bind(&hash_of_code)
    .bind(now.0)
    .fetch_optional(db)
    .await?;
    let role = role_from(&role.ok_or_else(invalid)?);
    let password_hash = hash_password(password).await?;
    let mut tx = db.begin().await?;
    let user = insert_user(&mut tx, now, &username, &display_name, role, &password_hash).await?;
    // Consume the code in the same transaction, so two concurrent accepts
    // cannot both succeed.
    let consumed = sqlx::query(
        "UPDATE invitations SET used_at = ?, used_by = ?
         WHERE code_hash = ? AND used_at IS NULL AND withdrawn_at IS NULL AND expires_at > ?",
    )
    .bind(now.0)
    .bind(user.0)
    .bind(&hash_of_code)
    .bind(now.0)
    .execute(&mut *tx)
    .await?;
    if consumed.rows_affected() != 1 {
        return Err(invalid());
    }
    tx.commit().await?;
    Ok(user)
}
