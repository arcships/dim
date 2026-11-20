import { defineAsyncComponent, h } from "vue"

/** `WebChatTimeline` in its own chunk; the shell it fills keeps the grid row while the chunk loads. */
export const WebChatTimeline = defineAsyncComponent({
  loader: () => import("./WebChatTimeline.vue"),
  loadingComponent: { render: () => h("div", { class: "timeline-shell" }) },
  delay: 0,
})
