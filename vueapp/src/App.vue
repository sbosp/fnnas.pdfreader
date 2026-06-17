<script setup>
</script>

<template>
  <router-view></router-view>
</template>

<style>
/* 全局CSS变量 */
:root {
  --bg: #f4f5f7;
  --panel: #ffffff;
  --border: #e3e6ea;
  --text: #1f2329;
  --sub: #8a9099;
  --accent: #2f6fed;
  --accent-soft: #eaf1fe;
  --shadow: 0 1px 3px rgba(0, 0, 0, .08), 0 6px 20px rgba(0, 0, 0, .04);
}

* {
  box-sizing: border-box;
}

html, body {
  margin: 0;
  height: 100vh;
  height: 100dvh;
  overflow: hidden;
  overscroll-behavior: none;
}

body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
  background: var(--bg);
  color: var(--text);
  -webkit-font-smoothing: antialiased;
}

#app {
  height: 100vh;
  height: 100dvh;
  display: flex;
  flex-direction: column;
  position: relative;
}

/* 按钮样式 */
.btn {
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text);
  border-radius: 8px;
  height: 34px;
  padding: 0 12px;
  font-size: 13px;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  user-select: none;
}

.btn:hover {
  background: #f7f8fa;
}

.btn:disabled {
  opacity: .5;
  cursor: default;
}

.btn.icon {
  padding: 0 10px;
}

/* 旋转动画 */
.spin {
  width: 26px;
  height: 26px;
  border: 3px solid #d8dde5;
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: r .8s linear infinite;
  display: inline-block;
  vertical-align: middle;
}

.spin.light {
  border-color: rgba(255, 255, 255, .25);
  border-top-color: #fff;
}

@keyframes r {
  to {
    transform: rotate(360deg);
  }
}

/* 响应式设计 */
@media (max-width: 640px) {
  .topbar .user {
    display: none;
  }
}
</style>
