plugins {
    kotlin("jvm") version "2.1.0"
    id("org.jetbrains.intellij.platform") version "2.5.0"
}

group = "dev.flux"
version = providers.gradleProperty("pluginVersion").getOrElse("0.1.0")

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        // Prefer a locally-installed IDE (no ~1 GB download, runs the exact IDE you have).
        // Set localIdePath in gradle.properties; otherwise fall back to downloading platformType/Version.
        val localIde = providers.gradleProperty("localIdePath").orNull
        if (!localIde.isNullOrBlank()) {
            local(localIde)
        } else {
            create(
                providers.gradleProperty("platformType").getOrElse("IC"),
                providers.gradleProperty("platformVersion").getOrElse("2024.3.5"),
            )
        }
    }
}

intellijPlatform {
    // Skip the headless searchable-options indexing pass: it's unnecessary for a dev/dogfood plugin and
    // collides with a running sandbox ("Only one instance of IDEA can be run at a time").
    buildSearchableOptions = false

    pluginConfiguration {
        // name + description live in META-INF/plugin.xml; Gradle owns version + compatibility range.
        version = project.version.toString()
        ideaVersion {
            sinceBuild = providers.gradleProperty("pluginSinceBuild").getOrElse("243")
            // Open-ended: don't cap the upper IDE build, so the plugin keeps loading
            // after IDE upgrades. Re-enable a cap before publishing to Marketplace.
            untilBuild = provider { null }
        }
    }
}

kotlin {
    // IntelliJ 2024.2+ runs on JBR 21 and must be built against Java 21.
    jvmToolchain(21)
}
