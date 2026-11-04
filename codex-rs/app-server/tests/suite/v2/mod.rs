mod agent_message_board;
mod analytics;
mod application_network;
#[path = "attached_thread_tests.rs"]
mod attached_thread;
#[path = "auth_storage_originator_tests.rs"]
mod auth_storage_originator;
mod auto_env;
#[path = "bedrock_service_tier_tests.rs"]
mod bedrock_service_tier;
mod client_metadata;
mod collaboration_mode_list;
#[cfg(unix)]
mod command_exec;
mod compaction;
#[path = "config_model_provider_requirements_tests.rs"]
mod config_model_provider_requirements;
mod config_requirements_application;
#[path = "config_requirements_browser_use_tests.rs"]
mod config_requirements_browser_use;
mod config_requirements_desktop;
#[path = "config_requirements_exec_tests.rs"]
mod config_requirements_exec;
mod config_requirements_in_app_browser;
mod config_requirements_login;
mod config_rpc;
#[path = "config_server_tests.rs"]
mod config_server;
#[cfg(unix)]
#[path = "connection_handling_stdio_tests.rs"]
mod connection_handling_stdio;
mod connection_handling_websocket;
#[path = "connection_handling_websocket_compression_tests.rs"]
mod connection_handling_websocket_compression;
#[cfg(unix)]
mod connection_handling_websocket_unix;
#[path = "critical_path_analytics_tests.rs"]
mod critical_path_analytics;
mod current_time;
mod cyber_access_program;
mod daemon_update_recovery;
mod dynamic_tools;
mod environment_add;
mod environment_info;
mod environment_status;
mod exec_server_test_support;
mod executor_mcp;
mod executor_skills;
mod experimental_api;
mod experimental_feature_list;
mod feedback;
mod frozen_cloud_skills;
mod fs;
mod hooks_list;
mod host_skills;
mod https_mock;
mod initialize;
mod mcp_connection_sharing;
mod mcp_resource;
mod mcp_server_status;
mod mcp_tool;
mod memory_read;
mod memory_reset;
mod misalignment_policy;
mod model_auto_review;
mod model_list;
mod model_list_requirements_tests;
mod model_provider_capabilities_read;
#[path = "model_provider_enforcement_tests.rs"]
mod model_provider_enforcement;
mod model_provider_list_tests;
mod multi_agent_v2_developer_instructions;
mod output_schema;
mod permission_profile_list;
mod plan_item;
mod process_exec;
mod projects;
mod rate_limit_reset_credits;
mod rate_limits;
#[cfg(debug_assertions)]
mod remote_thread_store;
mod request_permissions;
mod request_user_input;
mod request_validation;
mod residency;
mod review;
#[path = "rollout_compress_tests.rs"]
mod rollout_compress;
mod rollout_migration;
mod safety_check_downgrade;
mod selected_environment;
mod server_diagnostics;
mod session_end;
mod skills_list;
mod sleep;
mod sqlite_recovery;
mod thread_archive;
mod thread_attachments;
mod thread_delete;
mod thread_environments;
mod thread_fork;
#[path = "thread_fork_multi_agent_tests.rs"]
mod thread_fork_multi_agent;
mod thread_goal_empty_responses;
mod thread_inject_items;
mod thread_list;
mod thread_loaded_list;
mod thread_memory_mode_set;
mod thread_metadata_update;
mod thread_name_persistence;
mod thread_name_websocket;
mod thread_queue;
mod thread_read;
mod thread_read_state;
mod thread_resume;
mod thread_revert;
mod thread_sample;
mod thread_sections;
mod thread_settings_update;
mod thread_shell_command;
mod thread_start;
mod thread_status;
mod thread_timeline;
mod thread_unarchive;
mod thread_unload;
mod thread_unsubscribe;
mod turn_interrupt;
mod turn_settings_update;
mod turn_start;
mod turn_steer;
mod view_image;
mod web_search;

mod user_verification_mcp;
