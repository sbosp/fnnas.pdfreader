<script setup>
import {ref, computed, onMounted, onUnmounted, watch} from 'vue'
import {useRouter, useRoute} from 'vue-router'
import {request} from '@/utils/request.js'
import Folder from '@/components/Folder.vue'
import Book from '@/components/Book.vue'
import HistoryBook from '@/components/HistoryBook.vue'

const router = useRouter()
const route = useRoute()

const allBooks = ref([])
const recentBooks = ref([])
const username = ref('')

// 连续点击「用户」5 次后启用 vconsole
const userClickCount = ref(0)
let userClickTimer = null
const onUserClick = () => {
  userClickCount.value++
  if (userClickTimer) clearTimeout(userClickTimer)
  // 2 秒内未继续点击则重置
  userClickTimer = setTimeout(() => {
    userClickCount.value = 0
  }, 2000)

  if (userClickCount.value >= 5) {
    userClickCount.value = 0
    if (userClickTimer) {
      clearTimeout(userClickTimer)
      userClickTimer = null
    }
    if (typeof window.__enableVConsole === 'function') {
      Promise.resolve(window.__enableVConsole()).then(() => {
        console.log('✅ vConsole 已启用')
      })
    }
  }
}

watch(
    () => route.params,
    (newParams, oldParams) => {
      console.log('路由参数变化:', '从', oldParams, '到', newParams)
      console.log('当前路由对象:', router.currentRoute.value)
      // 对路由变化做出响应...
      console.log('路径发生变化，刷新页面数据', newParams.folderId)
      let folderId = newParams.folderId || ''
      refreshPage('', folderId)
    }
)

// 计算当前层级的数据
const currentLevel = computed(() => {
  const folders = []
  const files = []
  allBooks.value.forEach(function (b) {
    if (b.type === 'folder') {
      folders.push(b)
    } else if (b.type === 'file') {
      files.push(b)
    }
  })

  return {folders, files}
})

// 目录数据内存缓存：key=folderId('' 表示根)，返回上一级时立即命中，避免异步空窗导致的列表闪动
const pageCache = new Map()

const refreshPage = (scan = "", path = '') => {
  if (path === '') {
    path = route.params?.folderId || ''
  }

  // 命中缓存则先瞬时渲染，返回/切换目录时不再出现「旧列表→新列表」的跳变。
  // scan==='all'（手动刷新）时强制走网络，不吃缓存。
  if (scan !== 'all') {
    const cached = pageCache.get(path)
    if (cached) {
      allBooks.value = cached.books
      recentBooks.value = cached.history
      username.value = cached.username
    }
  }

  request.get(`books?path=${path}&scan=${scan}`).then((data) => {
    const books = data.data.books || []
    const history = data.data.history || []
    const uname = data.data.username || '用户'
    pageCache.set(path, {books, history, username: uname})
    allBooks.value = books
    recentBooks.value = history
    username.value = uname
  }).catch((err) => {
    console.error('❌ books请求失败:', {
      message: err.message,
      code: err.code,
      stack: err.stack,
      response: err.response ? {
        status: err.response.status,
        statusText: err.response.statusText,
        data: err.response.data,
        headers: err.response.headers
      } : null
    })
  })
}

const refreshClick = () => {
  router.replace('/')
  console.log('refreshClick')
  refreshPage('all')
}

const navTo = (path) => {
  console.log('执行路由跳转，目标路径:', path)
  if (!path) {
    // 回到根目录
    router.push('/')
  } else {
    // 导航到指定文件夹
    router.push(`/${path}`)
  }
}

const openBook = (book) => {
  console.log('点击文件:', book)
  navTo(`reader/${book.id}`)
}

const enterFolder = (folder) => {
  console.log('点击文件夹:', folder)
  navTo(`folder/${folder.id}`)
}

function back() {
  router.back()
}

// 初始加载
onMounted(() => {
  // 监听路由变化
  const unwatch = router.afterEach((to, from) => {
    console.log('路由跳转完成:', '从', from.path, '到', to.path)
    console.log('路由参数:', to.params)
  })

  // 组件卸载时取消监听
  onUnmounted(() => {
    unwatch()
  })
  refreshPage()
})
</script>

<template>
  <!-- 顶部栏 -->
  <div class="topbar">
    <span class="brand">
      <button class="btn" @click="back">← 返回</button>
      <svg viewBox="0 0 24 24" fill="none">
        <path d="M6 2h8l4 4v16H6z" stroke="#2f6fed" stroke-width="1.6" stroke-linejoin="round"/>
        <path d="M14 2v4h4" stroke="#2f6fed" stroke-width="1.6" stroke-linejoin="round"/>
        <path d="M8.5 13h7M8.5 16.5h7M8.5 9.5h3" stroke="#2f6fed" stroke-width="1.4" stroke-linecap="round"/>
      </svg>
      PDF 阅读器
    </span>
    <div class="spacer"></div>
    <button class="btn icon" @click="refreshClick" title="刷新书库">刷新</button>
    <span class="user" @click="onUserClick">{{ username }}</span>
  </div>

  <!-- 书架内容 -->
  <div class="shelf-wrap">
    <!-- 最近阅读 -->
    <div class="recent" v-if="recentBooks.length">
      <p class="rhead"><span class="ricon">🕘</span> 最近阅读</p>
      <div class="recent-strip">
        <div
            v-for="book in recentBooks"
            :key="book.id"
            class="ritem"
            @click="openBook(book)"
        >
          <HistoryBook
              :key="book.id"
              :book="book"
              @click="openBook(book)"
          />
        </div>
      </div>
    </div>

    <div class="shelf-head">
      <h2>我的书架</h2>
      <span class="count" v-if="allBooks.length">共 {{ allBooks.length }} 个</span>
    </div>
    <div v-if="!allBooks.length" class="empty">
      <svg viewBox="0 0 24 24" fill="none">
        <path d="M4 5h16v14H4z" stroke="currentColor" stroke-width="1.4"/>
        <path d="M9 5v14" stroke="currentColor" stroke-width="1.4"/>
      </svg>
      书架还是空的。<br/>请在「文件管理 → 应用文件 → PDF 阅读器 → PDFLibrary」中放入 PDF（可建子文件夹），<br/>或在「应用设置
      → 允许访问的文件夹」中添加目录，然后点右上角「⟳ 刷新」。
    </div>
    <div class="grid">
      <!-- 书籍 -->
      <Book
          v-for="book in currentLevel.files"
          :key="book.id"
          :book="book"
          @click="openBook(book)"
      />
    </div>
    <div style="height: 16px"/>
    <div class="grid">
      <!-- 文件夹 -->
      <Folder
          v-for="folder in currentLevel.folders"
          :key="folder.id"
          :folder="folder"
          @click="enterFolder(folder)"
      />
    </div>
  </div>

</template>

<style scoped>
/* 顶部栏样式 */
.topbar {
  height: 52px;
  flex: 0 0 52px;
  display: flex;
  align-items: center;
  padding: 0 16px;
  background: var(--panel);
  border-bottom: 1px solid var(--border);
  gap: 12px;
  z-index: 10;
}

.topbar .brand {
  font-weight: 600;
  font-size: 15px;
  display: flex;
  align-items: center;
  gap: 8px;
}

.topbar .brand svg {
  width: 20px;
  height: 20px;
}

.topbar .spacer {
  flex: 1;
}

.topbar .user {
  font-size: 13px;
  color: var(--sub);
  cursor: pointer;
  user-select: none;
}

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

.btn.icon {
  padding: 0 10px;
}

/* 书架样式 */
.shelf-wrap {
  flex: 1;
  overflow: auto;
  padding: 18px 22px 28px;
  overflow-anchor: none;
  overscroll-behavior: none;
  -webkit-overflow-scrolling: touch;
}

.shelf-head {
  display: flex;
  align-items: center;
  margin-bottom: 14px;
  gap: 12px;
  flex-wrap: wrap;
}

.shelf-head h2 {
  font-size: 17px;
  margin: 0;
}

.shelf-head .count {
  color: var(--sub);
  font-size: 13px;
}

/* 最近阅读 */
.recent {
  margin-bottom: 22px;
}

.recent .rhead {
  font-size: 14px;
  font-weight: 600;
  margin: 0 0 10px;
  display: flex;
  align-items: center;
  gap: 6px;
}

.recent .rhead .ricon {
  color: var(--accent);
}

.recent-strip {
  display: flex;
  gap: 12px;
  overflow-x: auto;
  overflow-y: hidden;
  padding: 2px 2px 10px;
  scroll-snap-type: x proximity;
  -webkit-overflow-scrolling: touch;
}

.recent-strip::-webkit-scrollbar {
  height: 6px;
}

.recent-strip::-webkit-scrollbar-thumb {
  background: #cfd5dd;
  border-radius: 3px;
}

.ritem {
  flex: 0 0 auto;
  width: 96px;
  cursor: pointer;
  scroll-snap-align: start;
  transition: transform .12s;
}

.ritem:hover {
  transform: translateY(-3px);
}

.ritem .rcover {
  width: 96px;
  height: 128px;
  border-radius: 9px;
  overflow: hidden;
  position: relative;
  background: linear-gradient(135deg, #e9eef7, #dde5f2);
  border: 1px solid var(--border);
  box-shadow: var(--shadow);
  display: flex;
  align-items: center;
  justify-content: center;
}

.ritem .rcover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

.ritem .rcover .rph svg {
  width: 34px;
  height: 34px;
}

.ritem .rbar {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  height: 3px;
  background: rgba(0, 0, 0, .12);
}

.ritem .rbar > i {
  display: block;
  height: 100%;
  background: var(--accent);
}

.ritem .rpage {
  position: absolute;
  right: 5px;
  top: 5px;
  background: rgba(47, 111, 237, .92);
  color: #fff;
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 999px;
}

.ritem .rname {
  font-size: 11px;
  line-height: 1.3;
  margin-top: 6px;
  color: var(--text);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  height: 29px;
}

/* 网格布局 */
.grid {
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(auto-fill, minmax(148px, 1fr));
}

/* 空状态 */
.empty {
  text-align: center;
  color: var(--sub);
  padding: 56px 20px;
  font-size: 14px;
  line-height: 1.7;
}

.empty svg {
  width: 56px;
  height: 56px;
  opacity: .4;
  display: block;
  margin: 0 auto 14px;
}

/* 响应式设计 */
@media (max-width: 640px) {
  .topbar {
    padding: 0 10px;
    gap: 8px;
  }

  /*手机端隐藏返回按钮*/
  .topbar .brand .btn {
    display: none;
  }

}
</style>

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
</style>