import sys, re
pom = sys.argv[1]
targets = sys.argv[2] if len(sys.argv) > 2 else None
s = open(pom).read()
plugin = f'''
      <plugin>
        <groupId>org.pitest</groupId>
        <artifactId>pitest-maven</artifactId>
        <version>1.17.1</version>
        <dependencies>
          <dependency>
            <groupId>org.pitest</groupId>
            <artifactId>pitest-junit5-plugin</artifactId>
            <version>1.2.1</version>
          </dependency>
        </dependencies>
        <configuration>
          <outputFormats><param>XML</param></outputFormats>
          <timestampedReports>false</timestampedReports>
          <fullMutationMatrix>true</fullMutationMatrix>
          <threads>4</threads>
          {f"<targetClasses><param>{targets}</param></targetClasses>" if targets else ""}
        </configuration>
      </plugin>
'''
# first <build> ... <plugins> not inside pluginManagement
m = re.search(r'<build>(.*?)</build>', s, re.S)
build = m.group(1)
pm = re.sub(r'<pluginManagement>.*?</pluginManagement>', lambda x: ' ' * len(x.group(0)), build, flags=re.S)
i = pm.find('<plugins>')
if i < 0:
    s = s[:m.start(1)] + '\n    <plugins>' + plugin + '    </plugins>\n' + s[m.start(1):]
else:
    at = m.start(1) + i + len('<plugins>')
    s = s[:at] + plugin + s[at:]
open(pom, 'w').write(s)
