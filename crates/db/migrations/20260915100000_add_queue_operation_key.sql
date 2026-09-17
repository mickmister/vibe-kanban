CREATE UNIQUE INDEX idx_agent_message_queue_operation_key
ON agent_message_queue(json_extract(data, '$.operation_key'))
WHERE json_extract(data, '$.operation_key') IS NOT NULL;
