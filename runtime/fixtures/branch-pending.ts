import { definePlugin } from '@arcships/rutis'
import counter from './branch-part.ts'

export default definePlugin({
  ...counter,
  inject: [...counter.inject, 'toolContribution:ts_echo'],
})
