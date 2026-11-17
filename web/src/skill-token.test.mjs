import assert from "node:assert/strict"
import test from "node:test"

import { namesSkillToken, skillSegments, skillTokens, startsWithSkillToken } from "./skill-token.ts"

const names = (text) => skillTokens(text).map((token) => token.name)

test("a token opens a text or follows whitespace", () => {
  assert.deepEqual(names("/skill:commit"), ["commit"])
  assert.deepEqual(names("/skill:commit please review"), ["commit"])
  assert.deepEqual(names("please /skill:commit it"), ["commit"])
  assert.deepEqual(names("one\n/skill:commit"), ["commit"])
  assert.deepEqual(names("/skill:alpha then /skill:beta"), ["alpha", "beta"])
  assert.deepEqual(skillTokens("hi /skill:commit"), [{ start: 3, end: 16, name: "commit" }])
})

test("a token never starts inside a word", () => {
  for (const text of ["a/skill:x", "https://host/skill:y", "/skill:", "/skill: ", "/skill", "/skills:x"]) {
    assert.deepEqual(names(text), [], text)
  }
})

test("a name is the run of name characters", () => {
  assert.deepEqual(names("/skill:a-b_c.d1"), ["a-b_c.d1"])
  assert.deepEqual(names("/skill:two words"), ["two"])
  assert.deepEqual(names("/skill:commit."), ["commit"], "a sentence's full stop stays text")
  assert.deepEqual(names("/skill:a.b"), ["a.b"], "a full stop inside a name stays in it")
  assert.deepEqual(names("/skill:a.."), ["a"], "trailing full stops end the token")
  assert.deepEqual(names("/skill:."), [], "a lone full stop is no name")
  assert.deepEqual(names("/skill:naïve"), ["na"])
  assert.deepEqual(names("/skill:<dim-skill>"), [])
  assert.ok(namesSkillToken("a-b_c.d1"))
  assert.ok(!namesSkillToken("two words"))
  assert.ok(!namesSkillToken(""))
  assert.ok(!namesSkillToken("naïve"))
  assert.ok(!namesSkillToken("trailing."), "a name ending in a full stop cannot be a token")
})

test("only a draft that begins with the token is a message, never a command", () => {
  assert.ok(startsWithSkillToken("/skill:commit fix it"))
  assert.ok(!startsWithSkillToken("please /skill:commit"))
  assert.ok(!startsWithSkillToken("/skill"))
})

test("a draft splits into its words with each token marked", () => {
  const segments = (text) => skillSegments(text).map((segment) => [segment.text, segment.skill])
  assert.deepEqual(segments("/skill:commit please"), [["/skill:commit", true], [" please", false]])
  assert.deepEqual(segments("run /skill:commit"), [["run ", false], ["/skill:commit", true]])
  assert.deepEqual(segments("/skill:alpha then /skill:beta"), [["/skill:alpha", true], [" then ", false], ["/skill:beta", true]])
  assert.deepEqual(segments("plain words"), [["plain words", false]])
  assert.deepEqual(segments(""), [], "an empty draft draws nothing")
  assert.deepEqual(segments("/skill:a/skill:b"), [["/skill:a", true], ["/skill:b", false]], "a token never starts inside a word")
})
