/*
 * Copyright (C) 2017 The Android Open Source Project
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

package com.android.utils

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class StringHelperTest {

  @Test
  fun testBasicCapitalize() {
    assertThat("foo".usLocaleCapitalize()).isEqualTo("Foo")
  }

  @Test
  fun testNonLetterCapitalize() {
    assertThat("1-Foo".usLocaleCapitalize()).isEqualTo("1-Foo")
  }

  @Test
  fun testUnicodeCapitalize() {
    assertThat("ê-foo".usLocaleCapitalize()).isEqualTo("Ê-foo")
  }

  @Test
  fun testSurrogateValuesCapitalize() {
    // this double characters is apparently not capitalizable.
    // FIXME find a better example...
    assertThat("\uD801\uDC00-foo".usLocaleCapitalize()).isEqualTo("\uD801\uDC00-foo")
  }

  @Test
  fun testAppendCapitalized() {
    assertThat("assemble".appendCapitalized("foo")).isEqualTo("assembleFoo")
  }

  @Test
  fun testAppendCapitalizedVarArgs() {
    assertThat("assemble".appendCapitalized("foo", "bar", "foo")).isEqualTo("assembleFooBarFoo")
  }

  @Test
  fun testDecapitalizeEmpty() {
    assertThat("".usLocaleDecapitalize()).isEqualTo("")
  }

  @Test
  fun testDecapitalizeNonLetter() {
    assertThat("1-Foo".usLocaleDecapitalize()).isEqualTo("1-Foo")
  }

  @Test
  fun testDecapitalizeUnicode() {
    assertThat("Ê-foo".usLocaleDecapitalize()).isEqualTo("ê-foo")
  }

  @Test
  fun testSurrogateValuesDecapitalize() {
    assertThat("\uD801\uDC00-foo".usLocaleDecapitalize()).isEqualTo("\uD801\uDC00-foo")
  }

  @Test
  fun testAsSeparatedListContains() {
    assertThat("foo".asSeparatedListContains("foo")).isTrue()
    assertThat("foo".asSeparatedListContains("bar")).isFalse()
    assertThat("foo,bar".asSeparatedListContains("barge")).isFalse()
    assertThat("fool,bar".asSeparatedListContains("foo")).isFalse()
    assertThat("fool,bar baz".asSeparatedListContains("bar", ", ")).isTrue()
    assertThat("foo,bar baz".asSeparatedListContains("baz", ", ")).isTrue()
  }

  @Test
  fun testStringBuilderAppendCapitalized() {
    assertThat(StringBuilder("prefix").appendCapitalized("word").toString()).isEqualTo("prefixWord")
    assertThat(StringBuilder().appendCapitalized("word").toString()).isEqualTo("Word")
    assertThat(StringBuilder().appendCapitalized("").toString()).isEqualTo("")
  }

  @Test
  fun testAppendCapitalizedOverloads() {
    assertThat("base".appendCapitalized("word")).isEqualTo("baseWord")
    assertThat("base".appendCapitalized("one", "two")).isEqualTo("baseOneTwo")
  }

  @Test
  fun testCapitalizeAndAppend() {
    assertThat("word".capitalizeAndAppend("Suffix")).isEqualTo("WordSuffix")
    assertThat("".capitalizeAndAppend("Suffix")).isEqualTo("Suffix")
  }

  @Test
  fun testAppendCamelCase() {
    assertThat(StringBuilder().appendCamelCase("foo").toString()).isEqualTo("foo")
    assertThat(StringBuilder("foo").appendCamelCase("bar").toString()).isEqualTo("fooBar")
    assertThat(StringBuilder("foo").appendCamelCase("Bar").toString()).isEqualTo("fooBar")
  }

  @Test
  fun testCombineAsCamelCase() {
    assertThat(listOf("foo", "bar", "baz").combineAsCamelCase()).isEqualTo("fooBarBaz")
    assertThat(listOf("Foo", "bar", "Baz").combineAsCamelCase()).isEqualTo("FooBarBaz")
    assertThat(emptyList<String>().combineAsCamelCase()).isEqualTo("")

    val objects = listOf(1, 2, 3)
    assertThat(combineAsCamelCase(objects) { "item$it" }).isEqualTo("item1Item2Item3")
  }

  @Test
  fun testToStrings() {
    assertThat(toStrings("a", "b")).containsExactly("a", "b").inOrder()
    assertThat(toStrings("a", listOf("b", "c"), 123)).containsExactly("a", "b", "c", "123").inOrder()
    assertThat(
        toStrings(
          object : Any() {
            override fun toString() = "custom"
          }
        )
      )
      .containsExactly("custom")
  }

  @Test
  fun testTokenizeCommandLine() {
    // raw = true includes quotes in the tokens
    assertThat("foo bar \"baz qux\"".tokenizeCommandLineToRaw()).containsExactly("foo", "bar", "\"baz qux\"").inOrder()

    // raw = false (escaped) removes quotes
    assertThat("foo bar \"baz qux\"".tokenizeCommandLineToEscaped()).containsExactly("foo", "bar", "baz qux").inOrder()
  }

  @Test
  fun testToSystemLineSeparator() {
    val systemLineSeparator = System.lineSeparator()
    assertThat("line1\nline2".toSystemLineSeparator()).isEqualTo("line1${systemLineSeparator}line2")
    assertThat("line1\r\nline2".toSystemLineSeparator()).isEqualTo("line1${systemLineSeparator}line2")
  }
}
