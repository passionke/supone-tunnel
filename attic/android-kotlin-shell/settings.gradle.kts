// Author: kejiqing
pluginManagement {
    repositories {
        val region = System.getenv("REGION") ?: ""
        if (region.equals("china", ignoreCase = true)) {
            maven(url = "https://maven.aliyun.com/repository/google")
            maven(url = "https://maven.aliyun.com/repository/public")
            maven(url = "https://maven.aliyun.com/repository/gradle-plugin")
        }
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        val region = System.getenv("REGION") ?: ""
        if (region.equals("china", ignoreCase = true)) {
            maven(url = "https://maven.aliyun.com/repository/google")
            maven(url = "https://maven.aliyun.com/repository/public")
        }
        google()
        mavenCentral()
    }
}

rootProject.name = "supone-tunnel"
include(":app")
