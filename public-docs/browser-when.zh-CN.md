---
title: 选择外部测试工具
description: 使用单独安装的工具测试项目中的网页。
nav: 外部测试工具
---

# 选择外部测试工具 {#choosing-external-test-tools}

原生 Flutter 应用没有内置 Paseo 浏览器。手动检查网页项目时使用普通外部浏览器，自动化测试则使用单独安装的 Playwright 等工具。优先复用项目已有的测试套件。

请区分以下情况：

- 网页项目的测试脚本是主机侧进程。Paseo 可以运行脚本并显示只读日志，不需要内嵌浏览器。
- 外部自动化工具可能安装浏览器二进制文件并使用自己的配置。这是用户选择的项目/工具依赖，不属于 Paseo 默认安装运行时。
- 远程服务 URL 必须能从运行浏览器的机器访问。手机上的 localhost 不是 daemon 主机。
- 项目或文档网站的浏览器测试不是原生 Flutter 客户端测试。

参见[外部浏览器工作流](/docs/browser)、[工作区脚本](/docs/worktrees#scripts-and-services)和[连接](/docs/connectivity)。
