import type { SourceDeclaration } from '../../../npm/plugin.js'

export const settings: SourceDeclaration = {
  "source": "web",
  "userFile": "web.json",
  "schema": {
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "searchService": {
        "type": "object",
        "required": [
          "kind"
        ],
        "additionalProperties": false,
        "properties": {
          "kind": {
            "type": "string",
            "minLength": 1
          },
          "endpoint": {
            "type": "string",
            "format": "uri"
          },
          "apiKey": {
            "type": "string",
            "minLength": 1
          }
        }
      },
      "allowPrivateHosts": {
        "type": "array",
        "items": {
          "type": "string"
        },
        "default": []
      },
      "proxy": {
        "type": "string",
        "format": "uri"
      }
    }
  },
  "userOnly": [
    "/searchService",
    "/allowPrivateHosts"
  ]
}
