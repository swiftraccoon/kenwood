public class DvUrlData
{
	private int m_a;

	private int m_b;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			this.m_a = value;
		}
	}

	public int StartAddress
	{
		set
		{
			this.m_b = value;
		}
	}

	public string Url
	{
		get { return string.Empty; }
	}

	public void b(n7 A_0, int A_1)
	{
		int a_ = this.m_b + this.m_a + 96 * A_1;
		A_0.d(Url, a_, 96);
	}
}
