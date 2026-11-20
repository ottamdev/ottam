-- The model and thought level the agent last confirmed for this session, on
-- the same terms as current_mode_id: its acknowledgment at new/load/apply, or
-- a change it announced. Restores and follow-ups that request neither keep
-- them, and the app shows them for the chat. A Run's requested model and level
-- stay that Run's start-time choice. NULL until observed, and for sessions
-- confirmed before this migration.
ALTER TABLE agent_sessions ADD COLUMN current_model_id TEXT;
ALTER TABLE agent_sessions ADD COLUMN current_thought_level_id TEXT;
