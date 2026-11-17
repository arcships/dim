import { definePlugin } from '@arcships/rutis'
import type { ManagementMethods, BasePromptSettingsHost, SkillsInstructionsSettingsHost, SummaryPromptSettingsHost } from '../../npm/plugin.js'

export default definePlugin({
  name: 'PromptSettingsFixture',
  provides: {},
  inject: ['managementMethods', 'basePromptSettingsHost', 'skillsInstructionsSettingsHost', 'summaryPromptSettingsHost'],
  async apply(ctx) {
    const base = ctx.use<BasePromptSettingsHost>('basePromptSettingsHost')
    const skills = ctx.use<SkillsInstructionsSettingsHost>('skillsInstructionsSettingsHost')
    const summary = ctx.use<SummaryPromptSettingsHost>('summaryPromptSettingsHost')
    await ctx.use<ManagementMethods>('managementMethods').register(
      'promptsettingsprobe',
      [{ name: 'promptsettingsprobe.read', mutating: false, interruption: 'report', progress: false }],
      async () => ({
        kind: 'value',
        value: { base: await base.base({}), skills: await skills.instructions({}), summary: await summary.systemPrompt({}) },
      }),
    )
  },
})
