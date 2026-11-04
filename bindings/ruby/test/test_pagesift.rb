# frozen_string_literal: true

# libpagesift Ruby 绑定测试：Ruby 标准库 minitest，不联网。
require 'minitest/autorun'
require_relative '../pagesift'

class PagesiftTest < Minitest::Test
  def test_version_looks_like_semver
    assert_match(/\A\d+\.\d+\.\d+\z/, Pagesift.version)
  end

  def test_convert_html
    doc = Pagesift.convert('<h1>Hi</h1><p>World</p>')
    assert_includes doc, '"success":true'
    assert_includes doc, '# Hi'
  end

  def test_parse_csv_bytes
    engine = Pagesift::Engine.create
    doc = engine.parse("name,score\nalice,1\nbob,2\n".b, 'scores.csv')
    assert_includes doc, '"success":true'
    assert_includes doc, '| alice | 1 |'
  ensure
    engine&.close
  end

  def test_unsupported_file_is_reported
    engine = Pagesift::Engine.create
    doc = engine.parse("\x00\x01\x02\x03".b, 'blob.xyz')
    assert_includes doc, '"success":false'
    assert_includes doc, '"error_code":"UNSUPPORTED_OPTION"'
  ensure
    engine&.close
  end

  def test_bad_url_is_reported
    engine = Pagesift::Engine.create
    assert_includes engine.scrape('not a url'), '"error_code":"INVALID_REQUEST"'
  ensure
    engine&.close
  end

  def test_unknown_option_is_rejected
    engine = Pagesift::Engine.create
    out = engine.scrape('http://example.invalid/', '{"no_such_option":1}')
    assert_includes out, '"error_code":"INVALID_REQUEST"'
  ensure
    engine&.close
  end

  def test_bad_engine_config_raises
    assert_raises(ArgumentError) { Pagesift::Engine.create('{"no_such_key":true}') }
  end

  def test_closed_engine_raises
    engine = Pagesift::Engine.create
    engine.close
    assert_raises(IOError) { engine.scrape('http://example.invalid/') }
  end
end
