<script setup lang="ts">
import { ref } from 'vue'
import type {
  ChatUiPlainTimelineSurfaceEmits,
  ChatUiPlainTimelineSurfaceProps,
} from './plain-timeline'

defineOptions({
  name: 'PlainTimelineSurface',
})

const props = defineProps<ChatUiPlainTimelineSurfaceProps>()

const emit = defineEmits<ChatUiPlainTimelineSurfaceEmits>()

const timelineRootRef = ref<HTMLElement | null>(null)
const scrollViewportRef = ref<HTMLElement | null>(null)
const timelineContentRef = ref<HTMLElement | null>(null)
const timelineFlowRef = ref<HTMLElement | null>(null)

/**
 * Scroll/restore math stays in the host adapter. The adapter reads these
 * elements through the exposed refs and keeps every listener, deadline, and
 * window-trim decision on its side.
 */
defineExpose({
  root: timelineRootRef,
  viewport: scrollViewportRef,
  content: timelineContentRef,
  flow: timelineFlowRef,
})
</script>

<template>
  <div
    ref="timelineRootRef"
    class="relative h-full min-h-0 w-full flex-1 shrink-0"
    :class="{ 'timeline--layout-resizing': props.state.layoutResizing, 'timeline--restoring': props.state.restoreTransitionSuppressed }"
    data-testid="chat-timeline"
    :data-timeline-prepend-anchor-outcome="props.diagnostics.prependAnchor?.outcome"
    :data-timeline-prepend-anchor-row-key="props.diagnostics.prependAnchor?.rowKey"
    :data-timeline-prepend-anchor-captured-top="props.diagnostics.prependAnchor?.capturedTopWithinViewportPx"
    :data-timeline-prepend-anchor-restored-top="props.diagnostics.prependAnchor?.restoredTopWithinViewportPx"
    :data-timeline-prepend-anchor-duration-ms="props.diagnostics.prependAnchor?.durationMs"
    :data-timeline-trim-state="props.diagnostics.trimState"
    :data-timeline-can-load-older="props.state.canLoadOlder ? 'true' : 'false'"
    :data-timeline-newer-gap="props.state.hasNewerGap ? 'true' : 'false'"
    :data-timeline-oldest-raw-message-id="props.diagnostics.oldestRawMessageId"
    :data-timeline-trim-anchor-key="props.diagnostics.trimAnchorKey"
    :data-timeline-trim-at-bottom="props.diagnostics.trimAtBottom ? 'true' : 'false'"
    :data-timeline-trim-restore-state="props.diagnostics.trimRestoreState"
    :data-timeline-trim-captured-top="props.diagnostics.trimCapturedTop ?? undefined"
    :data-timeline-trim-restored-top="props.diagnostics.trimRestoredTop ?? undefined"
    :data-render-window-start="props.diagnostics.renderWindowStart"
    :data-render-window-end="props.diagnostics.renderWindowEnd"
  >
    <div
      ref="scrollViewportRef"
      class="chat-scroll-viewport flex h-full min-h-0 flex-col overflow-y-auto overscroll-contain"
      :data-timeline-restore-covered="props.state.restoreCovered ? 'true' : undefined"
    >
      <div
        class="mx-auto flex min-h-full w-full max-w-4xl flex-col px-3 pt-6 pb-6"
        :data-timeline-message-count="props.state.messageCount"
        :data-timeline-row-count="props.state.rowCount"
      >
        <slot name="before-rows" />

        <div v-if="props.state.messageCount === 0 && !props.state.isRunning && !props.state.restoreCovered" class="flex min-h-full flex-1 items-center justify-center py-10 text-sm text-muted-foreground">
          {{ props.labels.emptySession }}
        </div>

        <div
          v-else
          ref="timelineFlowRef"
          class="relative min-h-full w-full"
        >
          <Transition
            enter-active-class="transition-opacity duration-200"
            leave-active-class="transition-opacity duration-150"
            enter-from-class="opacity-0"
            leave-to-class="opacity-0"
          >
            <div
              v-if="props.state.isLoadingOlder"
              class="timeline-history-loading"
              aria-live="polite"
              aria-busy="true"
            >
              <span class="sr-only">{{ props.labels.loading }}</span>
              <div class="timeline-history-loading__track">
                <div class="timeline-history-loading__bar" />
              </div>
            </div>
          </Transition>

          <Teleport
            v-if="props.state.showUserMessageRail && props.state.userMessageRailHost"
            :to="props.state.userMessageRailHost"
          >
            <component
              :is="props.components.userMessageRail"
              :session-id="props.state.sessionId"
              :markers="props.markers.markers"
              :loaded-markers="props.markers.loadedMarkers"
              :scroll-parent="scrollViewportRef"
              :timeline-root="timelineRootRef"
              :row-dom-signature="props.markers.rowDomSignature"
              :layout-resizing="props.state.layoutResizing"
              @marker-click="emit('marker-click', $event)"
              @wheel.prevent="emit('rail-wheel', $event)"
            />
          </Teleport>

          <div ref="timelineContentRef" class="timeline-plain-scroller" data-testid="timeline-plain-scroller">
            <div
              v-if="props.window.topSpacerPx > 0"
              class="timeline-row-placeholder w-full px-4"
              :style="{ height: `${props.window.topSpacerPx}px` }"
              data-timeline-row-spacer="top"
              aria-hidden="true"
            />
            <template v-for="(row, windowIndex) in props.window.rows" :key="row.key">
              <div
                class="timeline-row-shell w-full px-4"
                :class="{ 'space-y-4': row.kind === 'turn' }"
                :data-timeline-cv="props.window.contentVisibilityEnabled(row) ? 'true' : undefined"
                :style="props.window.contentVisibilityStyle(row)"
                :data-timeline-row-key="row.key"
                :data-timeline-row-message-backed="row.kind === 'turn' || row.kind === 'message' ? 'true' : undefined"
                data-timeline-row-active="true"
                :data-timeline-row-index="props.window.start + windowIndex"
              >
                <component :is="props.components.errorBoundary">
                  <component
                    :is="props.components.row"
                    :row="row"
                    :session-id="props.state.sessionId"
                    :is-latest-turn="row.kind === 'turn' && row.turn.id === props.rowState.latestTurnRowId"
                    :timeline-running="props.state.isRunning"
                    :turn-details-expanded="row.kind === 'turn'
                      ? (props.rowState.turnUiStateById[row.turn.id]?.detailsExpanded ?? false)
                      : false"
                    :turn-details-toggle-visible="row.kind === 'turn'
                      ? (props.rowState.turnUiStateById[row.turn.id]?.detailsToggleVisible ?? true)
                      : true"
                    :block-expanded-by-id="props.rowState.blockExpandedById"
                    :block-group-expanded-by-id="props.rowState.blockGroupExpandedById"
                    :assistant-user-message-id="row.kind === 'turn'
                      ? (props.rowState.assistantUserMessageIdByRowKey[row.key] ?? null)
                      : null"
                    :retry-user-message="row.kind === 'turn'
                      ? (row.turn.userMessage ?? props.rowState.userMessageById.get(props.rowState.assistantUserMessageIdByRowKey[row.key] ?? '') ?? null)
                      : null"
                    :search-query="props.rowState.searchQuery"
                    :search-matches-by-source="props.rowState.searchMatchesBySource"
                    :active-search-match-id="props.rowState.activeSearchMatchId"
                    :elapsed-now-ms="row.kind === 'turn' && row.turn.isRunning ? props.rowState.elapsedNowMs : undefined"
                    :markdown-viewport-priority="props.rowState.presentationActive"
                    :read-only="props.rowState.readOnly"
                    @toggle-details="emit('toggle-details', $event)"
                    @toggle-block="emit('toggle-block', $event)"
                    @toggle-block-group="emit('toggle-block-group', $event)"
                    @edit-message="emit('edit-message', $event)"
                    @retry-message="emit('retry-message', $event)"
                    @fork-message="emit('fork-message', $event)"
                    @branch-message="emit('branch-message', $event)"
                    @preview-file="emit('preview-file', $event)"
                    @commit="emit('commit', $event)"
                    @commit-and-push="emit('commit-and-push', $event)"
                    @push="emit('push')"
                    @diff-file="(path: string, messageId: string) => emit('diff-file', path, messageId)"
                    @open-agent="emit('open-agent', $event)"
                  />
                </component>
              </div>
            </template>
            <slot name="after-rows" />
            <div
              v-if="props.window.bottomSpacerPx > 0"
              class="timeline-row-placeholder w-full px-4"
              :style="{ height: `${props.window.bottomSpacerPx}px` }"
              data-timeline-row-spacer="bottom"
              aria-hidden="true"
            />
          </div>

          <div
            class="timeline-bottom-spacer mx-4 flex items-start text-base leading-6 text-muted-foreground"
            :data-testid="props.activity.visible ? 'timeline-running-placeholder' : undefined"
            data-timeline-activity-slot="true"
            data-scroll-follow-ignore="true"
            aria-live="polite"
            :aria-hidden="props.activity.visible ? undefined : 'true'"
          >
            <span v-if="props.activity.visible" class="timeline-activity-shimmer tabular-nums">{{ props.activity.text }}</span>
          </div>

          <div
            v-if="props.state.restoreCoverVisible"
            class="absolute inset-0 z-10 flex items-center justify-center bg-background"
            data-testid="timeline-restore-cover"
            aria-live="polite"
            aria-busy="true"
            @wheel.prevent
            @touchmove.prevent
          >
            <component :is="props.components.brandMark" animated class="h-12 text-muted-foreground" />
          </div>
        </div>
      </div>
    </div>

    <Transition
      enter-active-class="transition-opacity duration-150"
      leave-active-class="transition-opacity duration-150"
      enter-from-class="opacity-0"
      leave-to-class="opacity-0"
    >
      <button
        v-if="props.state.showScrollToBottom"
        type="button"
        data-testid="timeline-scroll-to-bottom"
        class="absolute bottom-4 left-1/2 z-30 flex h-8 w-8 -translate-x-1/2 items-center justify-center rounded-full border border-border bg-background shadow-md transition-colors hover:bg-accent"
        @click="emit('jump-to-bottom')"
      >
        <component :is="props.components.chevronDown" class="h-4 w-4 text-muted-foreground" />
      </button>
    </Transition>

    <Transition
      enter-active-class="transition-opacity duration-150"
      leave-active-class="transition-opacity duration-150"
      enter-from-class="opacity-0"
      leave-to-class="opacity-0"
    >
      <button
        v-if="props.state.hasNewerGap"
        type="button"
        data-testid="timeline-newer-gap"
        class="absolute top-3 left-1/2 z-30 flex max-w-[min(90%,32rem)] -translate-x-1/2 items-center gap-1.5 rounded-full border border-border bg-background/95 px-3 py-1.5 text-xs text-muted-foreground shadow-md backdrop-blur transition-colors hover:bg-accent hover:text-foreground"
        :aria-label="`${props.labels.newerHistoryGap} ${props.labels.returnToLatest}`"
        @click="emit('load-latest')"
      >
        <component :is="props.components.chevronUp" class="h-3.5 w-3.5 shrink-0" />
        <span class="truncate">{{ props.labels.newerHistoryGap }}</span>
        <span class="shrink-0 font-medium text-foreground">{{ props.labels.returnToLatest }}</span>
      </button>
    </Transition>
  </div>
</template>

<style scoped>
.chat-scroll-viewport {
  position: relative;
  scroll-behavior: auto;
  scrollbar-gutter: stable;
  scrollbar-width: thin;
  scrollbar-color: transparent transparent;
}

.chat-scroll-viewport[data-timeline-scrolled] {
  mask-image: linear-gradient(
    to bottom,
    transparent,
    #000 2rem
  );
}

.chat-scroll-viewport:hover,
.chat-scroll-viewport:focus,
.chat-scroll-viewport:focus-within,
.chat-scroll-viewport:active {
  scrollbar-color: var(--border) transparent;
}

.chat-scroll-viewport[data-timeline-restore-covered="true"],
.chat-scroll-viewport[data-timeline-restore-covered="true"]:hover,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:focus,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:focus-within,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:active {
  scrollbar-width: none;
  scrollbar-color: transparent transparent;
}

.chat-scroll-viewport::-webkit-scrollbar {
  width: 10px;
  height: 10px;
}

.chat-scroll-viewport[data-timeline-restore-covered="true"]::-webkit-scrollbar {
  width: 0;
  height: 0;
}

.chat-scroll-viewport::-webkit-scrollbar-track {
  background: transparent;
}

.chat-scroll-viewport::-webkit-scrollbar-corner {
  background: transparent;
}

.chat-scroll-viewport::-webkit-scrollbar-thumb {
  min-height: 24px;
  border: 1px solid transparent;
  border-radius: 9999px;
  background: transparent;
  background-clip: padding-box;
}

.chat-scroll-viewport:hover::-webkit-scrollbar-thumb,
.chat-scroll-viewport:focus::-webkit-scrollbar-thumb,
.chat-scroll-viewport:focus-within::-webkit-scrollbar-thumb,
.chat-scroll-viewport:active::-webkit-scrollbar-thumb {
  background: var(--border);
  background-clip: padding-box;
}

.chat-scroll-viewport[data-timeline-restore-covered="true"]::-webkit-scrollbar-thumb,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:hover::-webkit-scrollbar-thumb,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:focus::-webkit-scrollbar-thumb,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:focus-within::-webkit-scrollbar-thumb,
.chat-scroll-viewport[data-timeline-restore-covered="true"]:active::-webkit-scrollbar-thumb {
  background: transparent;
}

.timeline-plain-scroller :deep(pre) {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.timeline-row-shell {
  contain: layout;
}

.timeline-row-shell[data-timeline-cv='true'] {
  /* Completed rows skip off-screen layout/paint. `auto` remembers the last
     rendered size; the CSS variable is the first-layout fallback estimate. */
  content-visibility: auto;
  contain-intrinsic-size: auto var(--tl-row-estimate-height, 180px);
}

/* Let the browser anchor the scroll position while skipped rows realize
   (their placeholder heights become real sizes); without this the viewport
   content visibly shifts during the first scroll-through. */
.timeline-row-shell[data-timeline-cv='true'] {
  overflow-anchor: auto;
}

/* Send landing: the flow is a flex column whose min-height is the measured
   landing target. The zero-basis spacer absorbs the difference, so streaming
   shrinks it and any later content collapse grows it back in the same layout
   pass. The browser never sees the scroll range shorten under the viewport. */
/* The scroller is the flex host: rows are items, and the headroom spacer takes
   whatever the viewport has left after them. The blank is therefore a layout
   fact, not a JS-maintained min-height. The scroller keeps a real height (the
   viewport's) so its rows have content coordinates and the scroll range can
   reach the user message's top. */
.timeline-bottom-spacer {
  height: 40px;
}

.timeline--layout-resizing :deep([data-timeline-resize-stable-row]) {
  flex-wrap: nowrap;
  overflow: hidden;
}

.timeline--layout-resizing :deep([data-timeline-resize-stable-row] > *) {
  flex: 0 0 auto;
}

/* Restore mounts a session's settle window in frame-sized batches; the restore
   cover hides the sequence, so enter, collapse-grid and appear transitions have
   no viewer. Running them only turns mount work into layout/paint work and
   delays the first presented frame (31 BaseTransition instances were measured
   in one restore). The cover sits inside this subtree, so its own BrandMark
   animation is suppressed while it covers the restore; exclude the cover here
   if it ever has to move during a restore. */
.timeline--restoring .timeline-plain-scroller :deep(*) {
  transition: none !important;
  animation: none !important;
}

.timeline-history-loading {
  pointer-events: none;
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  height: 0;
  justify-content: center;
}

.timeline-history-loading__track {
  width: min(560px, calc(100% - 64px));
  height: 2px;
  margin-top: 4px;
  overflow: hidden;
  border-radius: 9999px;
  background: var(--muted);
}

.timeline-history-loading__bar {
  height: 100%;
  width: 42%;
  border-radius: inherit;
  background: color-mix(in oklab, var(--muted-foreground) 50%, transparent);
  animation: timeline-history-loading 950ms cubic-bezier(0.4, 0, 0.2, 1) infinite;
}

.timeline-activity-shimmer {
  position: relative;
  z-index: 1;
  background: linear-gradient(
    90deg,
    currentColor 0%,
    currentColor 40%,
    var(--activity-shimmer-highlight) 50%,
    currentColor 60%,
    currentColor 100%
  );
  background-size: 200% 100%;
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
  animation: timeline-activity-shimmer 2.4s steps(48, end) infinite;
}

@keyframes timeline-history-loading {
  0% {
    transform: translateX(-150%);
  }
  100% {
    transform: translateX(260%);
  }
}

@keyframes timeline-activity-shimmer {
  0% {
    background-position: 200% 0;
  }
  100% {
    background-position: -200% 0;
  }
}
</style>
