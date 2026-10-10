/*
 * Copyright (C) 2026 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
package com.android.testutils

import com.google.common.truth.Truth.assertThat
import java.io.File
import java.io.FileOutputStream
import java.util.ArrayDeque
import java.util.jar.Attributes
import java.util.jar.JarEntry
import java.util.jar.JarOutputStream
import java.util.jar.Manifest
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class TestGroupTest {

  @get:Rule val tmp = TemporaryFolder()

  private fun createJarWithClassPath(classPath: String): File {
    val jarFile = tmp.newFile("wrapper_${System.nanoTime()}.jar")
    val manifest = Manifest()
    manifest.mainAttributes[Attributes.Name.MANIFEST_VERSION] = "1.0"
    manifest.mainAttributes[Attributes.Name.CLASS_PATH] = classPath

    JarOutputStream(FileOutputStream(jarFile), manifest).use { target ->
      val entry = JarEntry("dummy.txt")
      target.putNextEntry(entry)
      target.write("dummy".toByteArray())
      target.closeEntry()
    }
    return jarFile
  }

  @Test
  fun testValidAbsolutePath() {
    val absTarget = tmp.newFile("abs_target.jar")
    val classPath = absTarget.toURI().toURL().toString()
    assertThat(classPath).startsWith("file:")
    val wrapperJar = createJarWithClassPath(classPath)
    val existingPaths = ArrayDeque<String>()

    TestGroup.addManifestClassPath(wrapperJar.absolutePath, existingPaths)

    assertThat(existingPaths).containsExactly(absTarget.absolutePath)
  }

  @Test
  fun testValidRelativePath() {
    val subDir = tmp.newFolder("sub")
    val wrapperJar = File(subDir, "wrapper.jar")
    val relativeName = "relative_target_${System.nanoTime()}.jar"
    val relTarget = File(subDir, relativeName)
    assertThat(relTarget.createNewFile()).isTrue()

    val inCwd = File(relativeName)
    assertThat(inCwd.exists()).isFalse()

    val manifest = Manifest()
    manifest.mainAttributes[Attributes.Name.MANIFEST_VERSION] = "1.0"
    manifest.mainAttributes[Attributes.Name.CLASS_PATH] = relativeName

    JarOutputStream(FileOutputStream(wrapperJar), manifest).use { target ->
      val entry = JarEntry("dummy.txt")
      target.putNextEntry(entry)
      target.write("dummy".toByteArray())
      target.closeEntry()
    }

    val existingPaths = ArrayDeque<String>()
    TestGroup.addManifestClassPath(wrapperJar.absolutePath, existingPaths)

    assertThat(existingPaths).containsExactly(relTarget.absolutePath)
  }
}
