ALTER TYPE chat_agent_type ADD VALUE IF NOT EXISTS 'observability_analyzer';

ALTER TYPE chat_message_type ADD VALUE IF NOT EXISTS 'tool_analyze_observability';
ALTER TYPE chat_message_type ADD VALUE IF NOT EXISTS 'tool_finish_observability_analysis';
