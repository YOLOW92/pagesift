# frozen_string_literal: true

# libpagesift 的 Ruby 绑定（fiddle，Ruby 标准库，无第三方 gem）。
# 方法返回 Document JSON 字符串，不解析。
# 库的加载顺序：环境变量 PAGESIFT_LIB（库文件完整路径）；然后 gem 里自带的库
# （lib/pagesift/native/<平台>/，有 x86_64-linux、aarch64-linux、arm64-darwin）；最后是系统库名。不联网下载。
require 'fiddle'
require 'rbconfig'

module Pagesift
  # 当前平台在 gem 里的目录名；不支持的平台是 nil
  def self.platform_dir
    cpu = case RbConfig::CONFIG['host_cpu']
          when /x86_64|amd64/ then 'x86_64'
          when /aarch64|arm64/ then 'aarch64'
          end
    case RbConfig::CONFIG['host_os']
    when /linux/ then cpu && "#{cpu}-linux"
    when /darwin/ then cpu == 'aarch64' ? 'arm64-darwin' : nil
    end
  end

  def self.lib_file_name
    RbConfig::CONFIG['host_os'] =~ /darwin/ ? 'libpagesift.dylib' : 'libpagesift.so'
  end

  def self.open_library
    explicit = ENV['PAGESIFT_LIB']
    return Fiddle.dlopen(explicit) if explicit && !explicit.empty?

    dir = platform_dir
    bundled = dir && File.join(__dir__, 'pagesift', 'native', dir, lib_file_name)
    return Fiddle.dlopen(bundled) if bundled && File.file?(bundled)

    Fiddle.dlopen(lib_file_name)
  end

  LIB = open_library

  VOID = Fiddle::TYPE_VOID
  PTR = Fiddle::TYPE_VOIDP
  LONG = Fiddle::TYPE_LONG

  def self.bind(name, args, ret)
    Fiddle::Function.new(LIB[name], args, ret)
  end

  VERSION_FN = bind('pagesift_version', [], PTR)
  STRING_FREE = bind('pagesift_string_free', [PTR], VOID)
  ENGINE_NEW = bind('pagesift_engine_new', [PTR, PTR], PTR)
  ENGINE_FREE = bind('pagesift_engine_free', [PTR], VOID)
  CONVERT = bind('pagesift_convert', [PTR, PTR], PTR)
  PARSE = bind('pagesift_parse', [PTR, PTR, LONG, PTR, PTR], PTR)
  SCRAPE = bind('pagesift_scrape', [PTR, PTR, PTR], PTR)

  # 把库返回的 char* 复制成 Ruby 字符串，并释放库里的那一份。
  def self.take(ptr)
    return nil if ptr.nil? || ptr.null?

    begin
      ptr.to_s.dup.force_encoding(Encoding::UTF_8)
    ensure
      STRING_FREE.call(ptr)
    end
  end

  # Ruby 字符串 → 以 NUL 结尾的 C 字符串（Fiddle::Pointer）；nil 为 NULL。
  def self.cstr(str)
    return nil if str.nil?

    Fiddle::Pointer.malloc(str.bytesize + 1, Fiddle::RUBY_FREE).tap do |p|
      p[0, str.bytesize] = str.b
      p[str.bytesize] = 0
    end
  end

  def self.version
    ptr = VERSION_FN.call
    ptr.to_s.dup.force_encoding(Encoding::UTF_8) # 静态字符串，不释放
  end

  # HTML 转 Markdown，不需要引擎。options_json 为 nil 时用默认值。
  def self.convert(html, options_json = nil)
    take(CONVERT.call(cstr(html), cstr(options_json)))
  end

  class Engine
    # config_json 为 nil 时用默认配置。失败抛 ArgumentError。
    def self.create(config_json = nil)
      err = Fiddle::Pointer.malloc(Fiddle::SIZEOF_VOIDP, Fiddle::RUBY_FREE)
      err[0, Fiddle::SIZEOF_VOIDP] = [0].pack('J')
      handle = ENGINE_NEW.call(Pagesift.cstr(config_json), err)
      if handle.null?
        msg = Pagesift.take(Fiddle::Pointer.new(err[0, Fiddle::SIZEOF_VOIDP].unpack1('J')))
        raise ArgumentError, msg || 'engine_new failed'
      end
      new(handle)
    end

    def initialize(handle)
      @handle = handle
      ObjectSpace.define_finalizer(self, self.class.send(:finalizer, handle))
    end

    # 解析内存里的文件，按内容和文件名识别类型。
    def parse(data, filename, options_json = nil)
      live!
      bytes = data.b
      buf = bytes.empty? ? nil : Fiddle::Pointer[bytes]
      Pagesift.take(PARSE.call(@handle, buf, bytes.bytesize, Pagesift.cstr(filename), Pagesift.cstr(options_json)))
    end

    def scrape(url, options_json = nil)
      live!
      Pagesift.take(SCRAPE.call(@handle, Pagesift.cstr(url), Pagesift.cstr(options_json)))
    end

    def close
      return unless @handle

      ObjectSpace.undefine_finalizer(self)
      ENGINE_FREE.call(@handle)
      @handle = nil
    end

    def self.finalizer(handle)
      proc { ENGINE_FREE.call(handle) }
    end
    private_class_method :finalizer

    private

    def live!
      raise IOError, 'engine is closed' if @handle.nil?
    end
  end
end
