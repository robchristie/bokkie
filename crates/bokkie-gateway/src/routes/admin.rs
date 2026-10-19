//! Admin user management, invitations, and an admin's view of members'
//! Bots. Every route needs an admin.
//!
//! Admins see accounts, limits, permissions and invitations. They make
//! Bots for a member and edit the profiles and grants of the Bots an admin
//! made for that member. They read a member's own Bots and any conversation only
//! while oversight of that member is on, and then read-only: no route here
//! changes a member's own Bot, or sends, steers or stops. Oversight never
//! applies to an admin, so no admin
//! reads another admin's Bots or conversations. Each change to a member's
//! account or Bots commits with its event in the member's own log.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use bokkie_protocol::api::{
    AdminUser, AdminUserList, Bot, CreateBotRequest, CreateInvitationRequest, CreatedInvitation,
    ErrorCode, InvitationList, MemberBot, MemberBotList, MessagePage, ResetPasswordRequest, Role,
    UpdateBotProfileRequest, UpdateUserRequest,
};
use bokkie_protocol::events::Event;
use bokkie_protocol::{BotId, MessageId, UserId};
use sqlx::sqlite::SqliteExecutor;

use crate::AppState;
use crate::accounts::{self, Account, AccountChanges};
use crate::auth::SignedInAdmin;
use crate::bots::store::{self, BotChanges, BotRow};
use crate::error::{Body, Failure, Path, Query, Result};
use crate::grants;
use crate::routes::bots::{PageQuery, to_api};

/// `GET /api/v1/admin/users`.
pub async fn list_users(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
) -> Result<Json<AdminUserList>> {
    Ok(Json(AdminUserList {
        users: accounts::admin_list_users(&state.db).await?,
    }))
}

/// `PATCH /api/v1/admin/users/{id}`.
pub async fn update_user(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path(id): Path<i64>,
    Body(request): Body<UpdateUserRequest>,
) -> Result<Json<AdminUser>> {
    // The change, the account's new state and its usage meter commit
    // together, in the account's own log.
    let mut batch = state.events.begin(UserId(id)).await?;
    let updated = accounts::admin_update_user(
        &mut batch,
        state.clock.now(),
        admin.id(),
        UserId(id),
        AccountChanges {
            display_name: request.display_name.as_deref(),
            disabled: request.disabled,
            limits: request.limits,
            permissions: request.permissions,
            oversight: request.oversight,
        },
    )
    .await?;
    // What the account's owner sees of it. Disabling needs no event: it
    // ends every session at once.
    if request.display_name.is_some()
        || request.permissions.is_some()
        || request.oversight.is_some()
    {
        batch
            .emit(Event::AccountUpdated {
                user: updated.user.clone(),
            })
            .await?;
    }
    if request.limits.is_some() {
        state.conversations.emit_usage(&mut batch).await?;
    }
    batch.commit().await?;
    if request.disabled == Some(true) {
        // A disabled account's Bots stop at once.
        state.supervisor.stop(UserId(id)).await;
    }
    tracing::info!(
        admin = %admin.id(),
        user = id,
        oversight = ?request.oversight,
        "updated an account"
    );
    Ok(Json(updated))
}

/// `PUT /api/v1/admin/users/{id}/password`.
pub async fn reset_password(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path(id): Path<i64>,
    Body(request): Body<ResetPasswordRequest>,
) -> Result<StatusCode> {
    let _permit = state.hashing.acquire().await.map_err(Failure::internal)?;
    accounts::admin_reset_password(
        &state.db,
        state.clock.now(),
        admin.id(),
        UserId(id),
        &request.new_password,
    )
    .await?;
    tracing::info!(admin = %admin.id(), user = id, "reset a password");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/admin/invitations`.
pub async fn create_invitation(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Body(request): Body<CreateInvitationRequest>,
) -> Result<(StatusCode, Json<CreatedInvitation>)> {
    let (id, code, expires_at) = accounts::admin_create_invitation(
        &state.db,
        state.clock.now(),
        admin.id(),
        request.role,
        request.permissions,
        request.display_name.as_deref(),
        request.expires_in_hours,
    )
    .await?;
    tracing::info!(admin = %admin.id(), invitation = id, "created an invitation");
    Ok((
        StatusCode::CREATED,
        Json(CreatedInvitation {
            id,
            code,
            role: request.role,
            expires_at,
        }),
    ))
}

/// `GET /api/v1/admin/invitations`.
pub async fn list_invitations(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
) -> Result<Json<InvitationList>> {
    Ok(Json(InvitationList {
        invitations: accounts::admin_list_invitations(&state.db, state.clock.now()).await?,
    }))
}

/// `DELETE /api/v1/admin/invitations/{id}`.
pub async fn withdraw_invitation(
    State(state): State<AppState>,
    _admin: SignedInAdmin,
    Path(id): Path<i64>,
) -> Result<StatusCode> {
    accounts::admin_withdraw_invitation(&state.db, state.clock.now(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// A member's Bots

/// The member an admin acts on. Another admin, or the admin themselves, is
/// refused: oversight never applies to an admin, and an admin's own Bots
/// have their own routes.
async fn member(db: impl SqliteExecutor<'_>, id: UserId) -> Result<Account> {
    let account = accounts::account(db, id)
        .await?
        .ok_or_else(Failure::not_found)?;
    if account.is_admin() {
        return Err(Failure::forbidden(
            "An admin's Bots and conversations are their own",
        ));
    }
    Ok(account)
}

/// Whether an admin may see this Bot of the member: every Bot while
/// oversight is on, else only those an admin made.
fn visible(member: &Account, bot: &BotRow) -> bool {
    member.user.oversight || bot.made_by.is_some()
}

fn member_bot(bot: &BotRow, overseen: Option<Bot>) -> MemberBot {
    MemberBot {
        id: bot.id,
        name: bot.name.clone(),
        label: bot.label.clone(),
        description: bot.description.clone(),
        avatar: bot.avatar.clone(),
        made_by_admin: bot.made_by.is_some(),
        created_at: bot.created_at,
        grants: bot.grants.iter().copied().collect(),
        overseen,
    }
}

/// `GET /api/v1/admin/users/{id}/bots`.
pub async fn member_bots(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path(id): Path<i64>,
) -> Result<Json<MemberBotList>> {
    // One read transaction, so the Bots shown are those oversight allowed
    // as it was when they were read.
    let mut tx = state.db.begin().await?;
    let member = member(&mut *tx, UserId(id)).await?;
    let oversight = member.user.oversight;
    let mut previews = if oversight {
        store::previews(&mut *tx, member.user.id).await?
    } else {
        Default::default()
    };
    let mut rows: Vec<BotRow> = store::list_bots(&mut *tx, member.user.id)
        .await?
        .into_iter()
        .filter(|bot| visible(&member, bot))
        .collect();
    drop(tx);
    if !oversight {
        // The member's own ordering reflects their pins and activity.
        rows.sort_by_key(|bot| bot.id);
    }
    let bots = rows
        .iter()
        .map(|bot| {
            let overseen = oversight.then(|| {
                bot.to_api(
                    state.conversations.is_working(bot.id),
                    previews.remove(&bot.id),
                )
            });
            member_bot(bot, overseen)
        })
        .collect();
    tracing::info!(admin = %admin.id(), user = id, oversight, "listed a member's Bots");
    Ok(Json(MemberBotList { oversight, bots }))
}

/// `POST /api/v1/admin/users/{id}/bots`: an admin makes a Bot for a member.
pub async fn create_member_bot(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path(id): Path<i64>,
    Body(request): Body<CreateBotRequest>,
) -> Result<(StatusCode, Json<MemberBot>)> {
    // The Bot and its event commit together, in the member's log.
    let mut batch = state.events.begin(UserId(id)).await?;
    let member = member(&mut *batch, UserId(id)).await?;
    let bot = store::create_bot(
        &mut *batch,
        state.clock.now(),
        member.user.id,
        Some(admin.id()),
        &request.name,
        &request.label,
        &request.description,
        request.avatar.as_deref(),
    )
    .await
    .map_err(for_member)?;
    let api = to_api(&state, &mut *batch, &bot).await?;
    batch.emit(Event::BotCreated { bot: api.clone() }).await?;
    batch.commit().await?;
    tracing::info!(admin = %admin.id(), user = id, bot = %bot.id, "made a Bot for a member");
    Ok((
        StatusCode::CREATED,
        Json(member_bot(&bot, member.user.oversight.then_some(api))),
    ))
}

/// `PATCH /api/v1/admin/users/{id}/bots/{bot}`: an admin changes the profile
/// or grants of a Bot an admin made for the member. The member's own Bots
/// are read-only to admins, even under oversight.
pub async fn update_member_bot(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path((id, bot_id)): Path<(i64, i64)>,
    Body(request): Body<UpdateBotProfileRequest>,
) -> Result<Json<MemberBot>> {
    if request == UpdateBotProfileRequest::default() {
        return Err(Failure::invalid(
            "Name the name, label, description, avatar or grants to change",
        ));
    }
    // A grant change waits for any send to the Bot, so that every turn runs
    // on a thread whose grants are the Bot's.
    let _sends = match request.grants {
        Some(_) => Some(
            state
                .conversations
                .lock_bot(UserId(id), BotId(bot_id))
                .await,
        ),
        None => None,
    };
    // The edit and its event commit together, in the member's log, and
    // oversight is read in the same transaction.
    let mut batch = state.events.begin(UserId(id)).await?;
    let member = member(&mut *batch, UserId(id)).await?;
    let existing = store::bot(&mut *batch, member.user.id, BotId(bot_id))
        .await?
        .filter(|bot| visible(&member, bot))
        .ok_or_else(Failure::not_found)?;
    if existing.made_by.is_none() {
        return Err(Failure::forbidden(
            "A member's own Bots are read-only to admins",
        ));
    }
    if let Some(requested) = &request.grants {
        let grants = grants::check_change(Role::Admin, &existing.grants, requested)?;
        if grants != existing.grants {
            store::set_grants(
                &mut *batch,
                state.clock.now(),
                member.user.id,
                existing.id,
                &grants,
            )
            .await?;
            tracing::info!(
                admin = %admin.id(),
                user = id,
                bot = bot_id,
                grants = %grants::to_stored(&grants),
                "set a member's Bot's grants"
            );
        }
    }
    let bot = store::update_bot(
        &mut *batch,
        state.clock.now(),
        member.user.id,
        BotId(bot_id),
        BotChanges {
            name: request.name,
            label: request.label,
            description: request.description,
            avatar: request.avatar,
            pinned: None,
            hidden: None,
        },
    )
    .await
    .map_err(for_member)?;
    let api = to_api(&state, &mut *batch, &bot).await?;
    batch.emit(Event::BotUpdated { bot: api.clone() }).await?;
    batch.commit().await?;
    tracing::info!(admin = %admin.id(), user = id, bot = bot_id, "edited a member's Bot");
    Ok(Json(member_bot(&bot, member.user.oversight.then_some(api))))
}

/// `GET /api/v1/admin/users/{id}/bots/{bot}/messages?before=&limit=`: a
/// member's conversation, read-only, only while oversight is on.
pub async fn member_messages(
    State(state): State<AppState>,
    SignedInAdmin(admin): SignedInAdmin,
    Path((id, bot_id)): Path<(i64, i64)>,
    Query(query): Query<PageQuery>,
) -> Result<Json<MessagePage>> {
    let seq = state.events.latest(UserId(id)).await?;
    // Oversight and the messages are read in one transaction, so a page is
    // never read after oversight was turned off.
    let mut tx = state.db.begin().await?;
    let member = member(&mut *tx, UserId(id)).await?;
    if !member.user.oversight {
        return Err(Failure::forbidden("Oversight of this account is off"));
    }
    store::bot(&mut *tx, member.user.id, BotId(bot_id))
        .await?
        .ok_or_else(Failure::not_found)?;
    let (messages, older) = store::messages(
        &mut *tx,
        member.user.id,
        BotId(bot_id),
        query.before.map(MessageId),
        query.limit.unwrap_or(50),
    )
    .await?;
    drop(tx);
    tracing::info!(
        admin = %admin.id(),
        user = id,
        bot = bot_id,
        "read a member's conversation under oversight"
    );
    Ok(Json(MessagePage {
        messages,
        older,
        seq,
    }))
}

/// Names the member, not the admin, in a duplicate-name conflict.
fn for_member(failure: Failure) -> Failure {
    if failure.code == ErrorCode::Conflict {
        Failure::conflict("That account already has a Bot with that name")
    } else {
        failure
    }
}
