-- For an app whose `audit_log` has `actor_user_id` and `target` columns, as oxi_feed
-- does. Copy this into one of the app's migrations, and move its audit calls to
-- `audit::record`. An old `target` value stays whole in `target_id`.
ALTER TABLE audit_log RENAME COLUMN actor_user_id TO user_id;
ALTER TABLE audit_log RENAME COLUMN target TO target_id;
ALTER TABLE audit_log ADD COLUMN target_type TEXT;
