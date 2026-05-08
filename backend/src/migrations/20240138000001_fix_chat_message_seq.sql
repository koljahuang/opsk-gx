-- Fix chat_messages seq ordering: old code saved all user messages with seq=0
-- and reset the counter per request, causing broken ordering in multi-turn sessions.
-- This migration reassigns seq values based on created_at order within each session.

WITH numbered AS (
    SELECT id, ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY created_at ASC, id ASC) AS new_seq
    FROM chat_messages
)
UPDATE chat_messages
SET seq = numbered.new_seq::INT
FROM numbered
WHERE chat_messages.id = numbered.id
  AND chat_messages.seq != numbered.new_seq::INT;
