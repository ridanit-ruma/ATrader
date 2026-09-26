-- Every account is visible to the connected agent; alerts go back to the conversation that set them.
ALTER TABLE accounts DROP COLUMN agent_id;
