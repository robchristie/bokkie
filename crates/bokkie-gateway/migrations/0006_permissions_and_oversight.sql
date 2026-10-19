-- Per-member permissions, the oversight switch, admin-made Bots and the
-- record of an admin's password reset. Existing accounts keep their
-- behaviour: both permissions on, oversight off, nothing to tell.

-- Whether the member may create Bots, and change their Bots' name, label,
-- description and avatar. Admins always have both.
ALTER TABLE users ADD COLUMN may_create_bots INTEGER NOT NULL DEFAULT 1
    CHECK (may_create_bots IN (0, 1));
ALTER TABLE users ADD COLUMN may_edit_bot_profiles INTEGER NOT NULL DEFAULT 1
    CHECK (may_edit_bot_profiles IN (0, 1));
-- While on, admins can read the member's Bots and conversations. Members
-- only; never on for an admin.
ALTER TABLE users ADD COLUMN oversight INTEGER NOT NULL DEFAULT 0
    CHECK (oversight IN (0, 1));
-- When oversight last changed; NULL if it never has.
ALTER TABLE users ADD COLUMN oversight_changed_at INTEGER;
-- When an admin last reset the member's password, until the member
-- acknowledges it; NULL when there is nothing to tell.
ALTER TABLE users ADD COLUMN password_reset_by_admin_at INTEGER;

-- The permissions an invitation gives the account it creates.
ALTER TABLE invitations ADD COLUMN may_create_bots INTEGER NOT NULL DEFAULT 1
    CHECK (may_create_bots IN (0, 1));
ALTER TABLE invitations ADD COLUMN may_edit_bot_profiles INTEGER NOT NULL DEFAULT 1
    CHECK (may_edit_bot_profiles IN (0, 1));

-- The admin who made the Bot for its owner; NULL when the owner made it.
ALTER TABLE bots ADD COLUMN made_by INTEGER REFERENCES users (id);
