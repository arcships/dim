import { createRouter, createWebHistory } from "vue-router"
import ChatPage from "./features/chat/ChatPage.vue"
import NewSessionPage from "./features/chat/NewSessionPage.vue"
import RootRedirect from "./views/RootRedirect.vue"
import NotFoundView from "./views/NotFoundView.vue"
import { useUiStore } from "./stores/ui"

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    // Reopens the last session, otherwise starts a new one.
    { path: "/", name: "root", component: RootRedirect },
    { path: "/chat/new", name: "chat-new", component: NewSessionPage },
    { path: "/chat/:sessionId", name: "chat-session", component: ChatPage, props: true },
    { path: "/:pathMatch(.*)*", name: "not-found", component: NotFoundView },
  ],
})

router.afterEach((to) => {
  if (to.name === "chat-session" && typeof to.params.sessionId === "string") useUiStore().lastSessionId = to.params.sessionId
})
