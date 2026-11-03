import type { SourceDeclaration } from '../../../npm/plugin.js'

export const settings: SourceDeclaration = {
  "source": "schedule",
  "userFile": "schedule.json",
  "userOnly": [
    "/max_concurrent_runs"
  ],
  "schema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "properties": {
      "jobs": {
        "default": [],
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "name": {
              "type": "string",
              "minLength": 1
            },
            "schedule": {
              "anyOf": [
                {
                  "type": "object",
                  "properties": {
                    "every_minutes": {
                      "type": "integer",
                      "minimum": 1,
                      "maximum": 9007199254740991
                    }
                  },
                  "required": [
                    "every_minutes"
                  ],
                  "additionalProperties": false
                },
                {
                  "type": "object",
                  "properties": {
                    "at": {
                      "type": "string",
                      "format": "date-time",
                      "pattern": "^(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))T(?:(?:[01]\\d|2[0-3]):[0-5]\\d(?::[0-5]\\d(?:\\.\\d+)?)?(?:Z|([+-](?:[01]\\d|2[0-3]):[0-5]\\d)))$"
                    }
                  },
                  "required": [
                    "at"
                  ],
                  "additionalProperties": false
                },
                {
                  "type": "object",
                  "properties": {
                    "cron": {
                      "type": "string",
                      "pattern": "^[0-9*/,-]+ +[0-9*/,-]+ +[0-9*/,-]+ +[0-9*/,-]+ +[0-9*/,-]+$"
                    },
                    "tz": {
                      "type": "string",
                      "minLength": 1
                    }
                  },
                  "required": [
                    "cron"
                  ],
                  "additionalProperties": false
                }
              ]
            },
            "prompt": {
              "type": "string",
              "maxLength": 32768
            },
            "cwd": {
              "type": "string",
              "minLength": 1
            },
            "model": {
              "type": "object",
              "properties": {
                "providerId": {
                  "type": "string",
                  "minLength": 1
                },
                "modelId": {
                  "type": "string",
                  "minLength": 1
                }
              },
              "required": [
                "providerId",
                "modelId"
              ],
              "additionalProperties": false
            },
            "mode": {
              "type": "string",
              "enum": [
                "create",
                "reuse"
              ]
            },
            "policy": {
              "type": "string",
              "enum": [
                "read-only",
                "workspace-write",
                "full-access"
              ]
            },
            "max_run_minutes": {
              "default": 60,
              "type": "integer",
              "minimum": 1,
              "maximum": 9007199254740991
            },
            "id": {
              "type": "string",
              "minLength": 1
            },
            "agent_id": {
              "type": "string",
              "minLength": 1
            },
            "session_id": {
              "type": "string",
              "minLength": 1
            },
            "policy_cap": {
              "type": "object",
              "properties": {
                "preset": {
                  "type": "string",
                  "enum": [
                    "read-only",
                    "workspace-write",
                    "full-access"
                  ]
                },
                "allowedTools": {
                  "type": "array",
                  "items": {
                    "type": "string"
                  }
                },
                "disallowedTools": {
                  "type": "array",
                  "items": {
                    "type": "string"
                  }
                }
              },
              "required": [
                "preset",
                "allowedTools",
                "disallowedTools"
              ],
            },
            "created_by": {
              "anyOf": [
                {
                  "type": "string",
                  "const": "user"
                },
                {
                  "type": "object",
                  "properties": {
                    "tool": {
                      "type": "object",
                      "properties": {
                        "session_id": {
                          "type": "string",
                          "minLength": 1
                        },
                        "branch_id": {
                          "type": "string",
                          "minLength": 1
                        }
                      },
                      "required": [
                        "session_id",
                        "branch_id"
                      ],
                      "additionalProperties": false
                    }
                  },
                  "required": [
                    "tool"
                  ],
                  "additionalProperties": false
                }
              ]
            },
            "enabled": {
              "type": "boolean"
            },
            "enabled_at": {
              "type": "string",
              "maxLength": 24,
              "format": "date-time",
              "pattern": "^(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))T(?:(?:[01]\\d|2[0-3]):[0-5]\\d:[0-5]\\d\\.\\d{3}(?:Z))$"
            },
            "revision": {
              "type": "integer",
              "minimum": 1,
              "maximum": 9007199254740991
            }
          },
          "required": [
            "name",
            "schedule",
            "prompt",
            "cwd",
            "mode",
            "max_run_minutes",
            "id",
            "policy_cap",
            "created_by",
            "enabled",
            "revision"
          ],
          "additionalProperties": false,
          "allOf": [
            {
              "if": {
                "properties": {
                  "enabled": {
                    "const": true
                  }
                },
                "required": [
                  "enabled"
                ]
              },
              "then": {
                "required": [
                  "enabled_at"
                ]
              },
              "else": {
                "not": {
                  "required": [
                    "enabled_at"
                  ]
                }
              }
            }
          ]
        }
      },
      "max_concurrent_runs": {
        "default": 4,
        "type": "integer",
        "minimum": 1,
        "maximum": 9007199254740991
      }
    },
    "required": [
      "jobs",
      "max_concurrent_runs"
    ],
    "additionalProperties": false
  }
}
