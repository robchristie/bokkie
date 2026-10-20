-- Counting a user's calls of one tool over a window, such as web_fetch's
-- fetches per day.
CREATE INDEX tool_calls_by_user_tool ON tool_calls (user_id, tool, at);
